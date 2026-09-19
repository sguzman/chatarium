    let completed = unsafe { EnumWindows(callback, (&mut search as *mut Search<'_>) as isize) };
    if completed == 0 {
        return Err(last_error("enumerate top-level windows"));
    }
    search.owners.sort_unstable();
    search.owners.dedup();
    Ok(search.owners)
}

fn job_process_ids(job: HANDLE) -> Result<Vec<u32>, String> {
    let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    // SAFETY: `accounting` is a correctly sized output structure.
    if unsafe {
        QueryInformationJobObject(
            job,
            JobObjectBasicAccountingInformation,
            (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
            size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(last_error("query Edge job process count"));
    }
    let mut capacity = (accounting.ActiveProcesses as usize).max(8);
    const MAX_RETRIES: usize = 8;
    for attempt in 0..MAX_RETRIES {
        if capacity > max_process_ids() {
            return Err(format!(
                "Edge job process membership exceeded limit {}",
                max_process_ids()
            ));
        }
        let size = 8usize
            .checked_add(
                capacity
                    .checked_mul(size_of::<usize>())
                    .ok_or_else(|| "Edge job process query size overflow".to_owned())?,
            )
            .ok_or_else(|| "Edge job process query size overflow".to_owned())?;
        let word_size = size_of::<usize>();
        let words = size
            .checked_add(word_size - 1)
            .ok_or_else(|| "Edge job process query size overflow".to_owned())?
            / word_size;
        // Back the C structure with pointer-aligned storage. Parsing remains byte-oriented
        // because the header contains two DWORDs followed by ULONG_PTR entries at offset 8.
        let mut storage = vec![0usize; words];
        let storage_len = storage.len() * word_size;
        let mut returned = 0;
        // SAFETY: storage is aligned for ULONG_PTR and large enough for the DWORD header plus
        // the requested trailing process-ID capacity.
        let ok = unsafe {
            QueryInformationJobObject(
                job,
                JobObjectBasicProcessIdList,
                storage.as_mut_ptr().cast(),
                storage_len as u32,
                &mut returned,
            )
        };
        // SAFETY: storage remains alive and unchanged while this read-only byte view is used.
        let bytes = unsafe {
            std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), storage_len)
        };
        let declared = u32::from_ne_bytes(bytes[4..8].try_into().unwrap()) as usize;
        if ok != 0 {
            return parse_process_id_list(bytes, word_size);
        }
        let error = unsafe { GetLastError() };
        if error != 122 && error != 234 {
            return Err(format!(
                "query Edge job process membership: Windows error {error}"
            ));
        }
        capacity = grow_capacity(capacity, declared, attempt)?;
    }
    Err("Edge job process membership did not stabilize within bounded retries".to_owned())
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn quote_windows_arg(argument: &str) -> String {
    if !argument.is_empty() && !argument.chars().any(|c| c.is_whitespace() || c == '"') {
        return argument.to_owned();
    }
    let mut quoted = String::from("\"");
    let mut slashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            slashes += 1;
        } else if character == '"' {
            quoted.push_str(&"\\".repeat(slashes * 2 + 1));
            quoted.push('"');
            slashes = 0;
        } else {
            quoted.push_str(&"\\".repeat(slashes));
            quoted.push(character);
            slashes = 0;
        }