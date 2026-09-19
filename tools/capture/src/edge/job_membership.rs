//! Parsing helpers for the variable trailing array in `JOBOBJECT_BASIC_PROCESS_ID_LIST`.

const HEADER_SIZE: usize = 8;
const MAX_PROCESS_IDS: usize = 16_384;

pub(super) fn parse_process_id_list(
    bytes: &[u8],
    pointer_width: usize,
) -> Result<Vec<u32>, String> {
    if pointer_width != 4 && pointer_width != 8 {
        return Err(format!("unsupported ULONG_PTR width: {pointer_width}"));
    }
    if bytes.len() < HEADER_SIZE {
        return Err("job process list is shorter than its two DWORD header fields".to_owned());
    }
    let count = u32::from_ne_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if count > MAX_PROCESS_IDS {
        return Err(format!(
            "job process list count {count} exceeds limit {MAX_PROCESS_IDS}"
        ));
    }
    let payload = count
        .checked_mul(pointer_width)
        .and_then(|size| HEADER_SIZE.checked_add(size))
        .ok_or_else(|| "job process list size overflow".to_owned())?;
    if bytes.len() < payload {
        return Err(format!(
            "job process list declares {count} IDs but provides {}",
            bytes.len().saturating_sub(HEADER_SIZE) / pointer_width
        ));
    }
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        let offset = HEADER_SIZE + index * pointer_width;
        let value = if pointer_width == 4 {
            u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap()) as u64
        } else {
            u64::from_ne_bytes(bytes[offset..offset + 8].try_into().unwrap())
        };
        let pid = u32::try_from(value)
            .map_err(|_| format!("job process ID {value} does not fit in u32"))?;
        ids.push(pid);
    }
    Ok(ids)
}

pub(super) const fn max_process_ids() -> usize {
    MAX_PROCESS_IDS
}

pub(super) fn grow_capacity(
    current: usize,
    declared: usize,
    retry: usize,
) -> Result<usize, String> {
    if retry >= 8 {
        return Err("job process membership did not stabilize within bounded retries".to_owned());
    }
    let next = declared.max(current.saturating_mul(2));
    if next <= current || next > MAX_PROCESS_IDS {
        return Err(
            "job process membership growth exceeded the bounded allocation limit".to_owned(),
        );
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::parse_process_id_list;

    fn buffer(assigned: u32, ids: &[u64], width: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(8 + ids.len() * width);
        bytes.extend_from_slice(&assigned.to_ne_bytes());
        bytes.extend_from_slice(&(ids.len() as u32).to_ne_bytes());
        for id in ids {
            if width == 4 {
                bytes.extend_from_slice(&(*id as u32).to_ne_bytes());
            } else {
                bytes.extend_from_slice(&id.to_ne_bytes());
            }
        }
        bytes
    }

    #[test]
    fn parses_documented_x64_layout_without_treating_first_pid_as_count() {
        assert_eq!(
            parse_process_id_list(&buffer(3, &[101, 202, 303], 8), 8).unwrap(),
            [101, 202, 303]
        );
    }

    #[test]
    fn empty_list_is_valid() {
        assert!(
            parse_process_id_list(&buffer(0, &[], 8), 8)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn uses_ids_in_list_count_when_assigned_count_differs() {
        let mut bytes = buffer(9, &[101, 202], 8);
        bytes[4..8].copy_from_slice(&2u32.to_ne_bytes());
        assert_eq!(parse_process_id_list(&bytes, 8).unwrap(), [101, 202]);
    }

    #[test]
    fn rejects_truncated_and_excessive_buffers() {
        assert!(parse_process_id_list(&[0; 7], 8).is_err());
        assert!(parse_process_id_list(&buffer(1, &[101], 8)[..9], 8).is_err());
        let mut bytes = vec![0u8; 8];
        bytes[4..8].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert!(parse_process_id_list(&bytes, 8).is_err());
    }

    #[test]
    fn rejects_unrepresentable_pid() {
        assert!(parse_process_id_list(&buffer(1, &[u64::from(u32::MAX) + 1], 8), 8).is_err());
    }

    #[test]
    fn bounded_growth_retries_until_stable_and_then_stops() {
        assert_eq!(super::grow_capacity(8, 12, 0).unwrap(), 16);
        assert!(super::grow_capacity(8, 12, 8).is_err());
        assert!(
            super::grow_capacity(super::max_process_ids(), super::max_process_ids() + 1, 0)
                .is_err()
        );
    }
}
