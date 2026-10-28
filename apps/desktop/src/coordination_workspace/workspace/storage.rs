//! storage methods for human-controlled coordination containers.
use super::super::*;

impl Workspace {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self {
                containers: Vec::new(),
                wake_leases: Vec::new(),
                next_id: 1,
            });
        }
        let data =
            fs::read(path).map_err(|e| format!("cannot read coordination workspace: {e}"))?;
        if data.len() > MAX_FILE {
            return Err("coordination workspace exceeds 16 MiB".to_owned());
        }
        let root: Value = serde_json::from_slice(&data)
            .map_err(|e| format!("invalid coordination workspace JSON: {e}"))?;
        if root["schema"] != SCHEMA || root["version"] != 1 {
            return Err("unknown coordination workspace schema".to_owned());
        }
        let mut containers = Vec::new();
        let mut ids = std::collections::BTreeSet::new();
        // Allocation is workspace-wide, including containers, members and
        // messages. Conversation identity is also unique across containers.
        let mut conversations = std::collections::BTreeSet::new();
        let mut largest = 0_u64;
        for c in root["containers"]
            .as_array()
            .ok_or("missing coordination containers")?
        {
            let id = number(c, "id")?;
            if id == 0 || !ids.insert(id) {
                return Err("duplicate container identity".to_owned());
            }
            largest = largest.max(id);
            let name = clean_label(required(c, "name")?, 128)?;
            let mut members = Vec::new();
            let mut member_ids = std::collections::BTreeSet::new();
            for m in c["members"].as_array().ok_or("missing members")? {
                let worker_id = number(m, "id")?;
                let conversation = clean_label(required(m, "conversation")?, 128)?;
                if worker_id == 0
                    || !ids.insert(worker_id)
                    || !member_ids.insert(worker_id)
                    || !conversations.insert(conversation.clone())
                {
                    return Err("duplicate worker or conversation membership".to_owned());
                }
                largest = largest.max(worker_id);
                members.push(Member {
                    id: worker_id,
                    conversation,
                    name: clean_label(required(m, "name")?, 128)?,
                    role: clean_label(required(m, "role")?, 128)?,
                });
            }
            let mut links = Vec::new();
            let mut edges = std::collections::BTreeSet::new();
            for l in c["links"].as_array().ok_or("missing topology links")? {
                let from = number(l, "from")?;
                let to = number(l, "to")?;
                let mode = required(l, "mode")?;
                if !member_ids.contains(&from)
                    || !member_ids.contains(&to)
                    || from == to
                    || !edges.insert((from, to))
                    || !mode_ok(mode)
                {
                    return Err("invalid or duplicate authority edge".to_owned());
                }
                let per_minute =
                    u32::try_from(number(l, "per_minute")?).map_err(|e| e.to_string())?;
                if per_minute > 600 {
                    return Err("invalid rate limit".to_owned());
                }
                links.push(Link {
                    from,
                    to,
                    mode: mode.to_owned(),
                    per_minute,
                    directives: field_bool(l, "directives")?,
                    reports: field_bool(l, "reports")?,
                });
            }
            let mut messages = Vec::new();
            for m in c["messages"]
                .as_array()
                .ok_or("missing coordination messages")?
            {
                let id = number(m, "id")?;
                let from = number(m, "from")?;
                let to = number(m, "to")?;
                let kind = required(m, "kind")?;
                let status = required(m, "status")?;
                let body = required(m, "body")?;
                if !member_ids.contains(&from)
                    || !member_ids.contains(&to)
                    || from == to
                    || !kind_ok(kind)
                    || body.len() > MAX_MESSAGE
                    || !matches!(
                        status,
                        "delivered"
                            | "consumed"
                            | "queued"
                            | "approval"
                            | "blocked"
                            | "rejected"
                            | "deleted"
                    )
                    || !ids.insert(id)
                    || id == 0
                {
                    return Err("invalid or duplicate coordination message".to_owned());
                }
                largest = largest.max(id);
                messages.push(Message {
                    id,
                    from,
                    to,
                    kind: kind.to_owned(),
                    body: body.to_owned(),
                    status: status.to_owned(),
                    created_ms: number(m, "created_ms")?,
                    delivered_ms: m["delivered_ms"].as_u64(),
                    consumed_ms: m["consumed_ms"].as_u64(),
                    note: required(m, "note")?.to_owned(),
                });
            }
            containers.push(Container {
                id,
                name,
                members,
                links,
                messages,
            });
        }
        let wake_leases = root
            .get("wake_leases")
            .map(|value| value.as_array().ok_or("wake_leases must be an array"))
            .transpose()?;
        let mut leases = Vec::new();
        let mut owners = std::collections::BTreeSet::new();
        if let Some(entries) = wake_leases {
            for entry in entries {
                let conversation = clean_label(required(entry, "conversation")?, 128)?;
                let remaining = u32::try_from(number(entry, "remaining")?)
                    .map_err(|_| "invalid wake allowance")?;
                if remaining > MAX_WAKE_ALLOWANCE
                    || !conversations.contains(&conversation)
                    || !owners.insert(conversation.clone())
                {
                    return Err("invalid wake lease owner or allowance".to_owned());
                }
                let claimed = entry["claimed_message_ids"]
                    .as_array()
                    .ok_or("missing wake claim history")?;
                if claimed.len() > MAX_WAKE_CLAIMS {
                    return Err("wake claim history exceeds its bound".to_owned());
                }
                let (_, worker) = containers
                    .iter()
                    .find_map(|c| {
                        c.members
                            .iter()
                            .find(|m| m.conversation == conversation)
                            .map(|m| (c, m.id))
                    })
                    .ok_or("wake lease references unknown worker")?;
                let mut claimed_ids = Vec::new();
                let mut unique = std::collections::BTreeSet::new();
                for id in claimed {
                    let id = id.as_u64().ok_or("invalid wake claim ID")?;
                    if !unique.insert(id)
                        || !containers
                            .iter()
                            .any(|c| c.messages.iter().any(|m| m.id == id && m.to == worker))
                    {
                        return Err("wake claim references duplicate or foreign message".to_owned());
                    }
                    claimed_ids.push(id);
                }
                let active = match entry.get("active_message_id") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(value.as_u64().ok_or("invalid active wake claim")?),
                };
                if active.is_some_and(|id| !unique.contains(&id)) {
                    return Err("active wake claim is absent from durable claim history".to_owned());
                }
                leases.push(WakeLease {
                    conversation,
                    remaining,
                    active_message_id: active,
                    claimed_message_ids: claimed_ids,
                });
            }
        }
        let next_id = number(&root, "next_id")?;
        if next_id == 0 || next_id <= largest {
            return Err("coordination ID sequence cannot be reused".to_owned());
        }
        Ok(Self {
            containers,
            wake_leases: leases,
            next_id,
        })
    }
    pub fn save_atomic(&self, path: &Path) -> Result<(), String> {
        let root = json!({
            "schema": SCHEMA,"version":1,"next_id":self.next_id,
            "wake_leases": self.wake_leases.iter().map(|lease| json!({
                "conversation":lease.conversation,"remaining":lease.remaining,
                "active_message_id":lease.active_message_id,
                "claimed_message_ids":lease.claimed_message_ids,
            })).collect::<Vec<_>>(),
            "containers":self.containers.iter().map(|c| json!({
                "id":c.id,"name":c.name,
                "members":c.members.iter().map(|m|json!({"id":m.id,"conversation":m.conversation,"name":m.name,"role":m.role})).collect::<Vec<_>>(),
                "links":c.links.iter().map(|l|json!({"from":l.from,"to":l.to,"mode":l.mode,"directives":l.directives,"reports":l.reports,"per_minute":l.per_minute})).collect::<Vec<_>>(),
                "messages":c.messages.iter().map(|m|json!({"id":m.id,"from":m.from,"to":m.to,"kind":m.kind,"body":m.body,"status":m.status,"created_ms":m.created_ms,"delivered_ms":m.delivered_ms,"consumed_ms":m.consumed_ms,"note":m.note})).collect::<Vec<_>>()
            })).collect::<Vec<_>>()
        });
        let bytes = serde_json::to_vec_pretty(&root).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_FILE {
            return Err("coordination workspace is full".to_owned());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        fs::rename(tmp, path).map_err(|e| e.to_string())
    }
}
