//! Explicit human-owned coordination containers for local conversations.
//! Worker identity and authority come from persisted membership, never model text.
//! Delivery is a durable inbox entry, not a forged user-authored chat message.
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Stroke, Vec2};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA: &str = "chatarium-coordination-workspace";
const MAX_MESSAGE: usize = 8192;
const MAX_FILE: usize = 16 * 1024 * 1024;
pub const FILE_NAME: &str = "coordination-workspace.json";

#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub id: u64,
    pub conversation: String,
    pub name: String,
    pub role: String,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub from: u64,
    pub to: u64,
    pub directives: bool,
    pub reports: bool,
    pub mode: String,
    pub per_minute: u32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub id: u64,
    pub from: u64,
    pub to: u64,
    pub kind: String,
    pub body: String,
    pub status: String,
    pub created_ms: u64,
    pub delivered_ms: Option<u64>,
    pub consumed_ms: Option<u64>,
    pub note: String,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Container {
    pub id: u64,
    pub name: String,
    pub members: Vec<Member>,
    pub links: Vec<Link>,
    pub messages: Vec<Message>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Workspace {
    pub containers: Vec<Container>,
    next_id: u64,
}

#[derive(Clone, Debug)]
pub struct UiDrafts {
    pub new_container: String,
    pub selected_container: u64,
    pub selected_conversation: String,
    pub worker_name: String,
    pub worker_role: String,
    pub source: u64,
    pub target: u64,
    pub send_from: u64,
    pub send_to: u64,
    pub mode: String,
    pub directives: bool,
    pub reports: bool,
    pub per_minute: u32,
    pub kind: String,
    pub body: String,
    pub notice: String,
    pub confirm_delete: Option<u64>,
}
impl Default for UiDrafts {
    fn default() -> Self {
        Self {
            new_container: String::new(),
            selected_container: 0,
            selected_conversation: String::new(),
            worker_name: String::new(),
            worker_role: "Worker".to_owned(),
            source: 0,
            target: 0,
            send_from: 0,
            send_to: 0,
            mode: "allow".to_owned(),
            directives: true,
            reports: true,
            per_minute: 12,
            kind: "directive".to_owned(),
            body: String::new(),
            notice: String::new(),
            confirm_delete: None,
        }
    }
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
fn clean_label(value: &str, max: usize) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > max || value.contains('\0') {
        return Err("label must be nonempty, UTF-8 and within its length limit".to_owned());
    }
    Ok(value.to_owned())
}
fn required<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("coordination data missing {key}"))
}
fn number(v: &Value, key: &str) -> Result<u64, String> {
    v.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("coordination data missing {key}"))
}
fn field_bool(v: &Value, key: &str) -> Result<bool, String> {
    v.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("coordination data missing {key}"))
}
fn mode_ok(mode: &str) -> bool {
    matches!(mode, "allow" | "block" | "queue" | "approve" | "rate_limit")
}
fn kind_ok(kind: &str) -> bool {
    matches!(kind, "directive" | "report" | "request" | "result" | "ack")
}
impl Workspace {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self {
                containers: Vec::new(),
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
            let mut local_ids = std::collections::BTreeSet::new();
            for m in c["members"].as_array().ok_or("missing members")? {
                let worker_id = number(m, "id")?;
                let conversation = clean_label(required(m, "conversation")?, 128)?;
                if worker_id == 0
                    || !member_ids.insert(worker_id)
                    || !local_ids.insert(conversation.clone())
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
            let mut message_ids = std::collections::BTreeSet::new();
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
                        "delivered" | "consumed" | "queued" | "approval" | "blocked" | "rejected" | "deleted"
                    )
                    || !message_ids.insert(id)
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
        let next_id = number(&root, "next_id")?;
        if next_id == 0 || next_id <= largest {
            return Err("coordination ID sequence cannot be reused".to_owned());
        }
        Ok(Self {
            containers,
            next_id,
        })
    }
    pub fn save_atomic(&self, path: &Path) -> Result<(), String> {
        let root = json!({
            "schema": SCHEMA,"version":1,"next_id":self.next_id,
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
    fn allocate(&mut self) -> u64 {
        let id = self.next_id.max(1);
        self.next_id = id.saturating_add(1);
        id
    }
    pub fn create_container(&mut self, name: &str) -> Result<u64, String> {
        let name = clean_label(name, 128)?;
        let id = self.allocate();
        self.containers.push(Container {
            id,
            name,
            members: vec![],
            links: vec![],
            messages: vec![],
        });
        Ok(id)
    }
    pub fn attach(
        &mut self,
        container: u64,
        conversation: &str,
        name: &str,
        role: &str,
    ) -> Result<u64, String> {
        let conversation = clean_label(conversation, 128)?;
        let name = clean_label(name, 128)?;
        let role = clean_label(role, 128)?;
        if self
            .containers
            .iter()
            .any(|c| c.members.iter().any(|w| w.conversation == conversation))
        {
            return Err("a conversation is already assigned to a coordination worker".to_owned());
        }
        if !self.containers.iter().any(|c| c.id == container) {
            return Err("container not found".to_owned());
        }
        let id = self.allocate();
        let c = self
            .containers
            .iter_mut()
            .find(|c| c.id == container)
            .unwrap();
        c.members.push(Member {
            id,
            conversation,
            name,
            role,
        });
        Ok(id)
    }
    pub fn set_link(
        &mut self,
        container: u64,
        from: u64,
        to: u64,
        directives: bool,
        reports: bool,
        mode: &str,
        per_minute: u32,
    ) -> Result<(), String> {
        if from == to || !mode_ok(mode) || per_minute > 600 {
            return Err("invalid topology edge or rate limit".to_owned());
        }
        let c = self
            .containers
            .iter_mut()
            .find(|c| c.id == container)
            .ok_or("container not found")?;
        if !c.members.iter().any(|m| m.id == from) || !c.members.iter().any(|m| m.id == to) {
            return Err("sender and recipient must both belong to this container".to_owned());
        }
        let link = Link {
            from,
            to,
            directives,
            reports,
            mode: mode.to_owned(),
            per_minute,
        };
        if let Some(old) = c.links.iter_mut().find(|l| l.from == from && l.to == to) {
            *old = link;
        } else {
            c.links.push(link);
        }
        Ok(())
    }
    /// Source identity is resolved by the application from its active conversation.
    /// A model may choose a target but cannot provide or override its sender identity.
    pub fn send(
        &mut self,
        conversation: &str,
        to: u64,
        kind: &str,
        body: &str,
        now: u64,
    ) -> Result<(u64, String), String> {
        if !kind_ok(kind) {
            return Err("unknown coordination message kind".to_owned());
        }
        if body.is_empty() || body.len() > MAX_MESSAGE || body.contains('\0') {
            return Err("coordination message must contain 1–8192 bytes".to_owned());
        }
        let (container, from) = self
            .containers
            .iter()
            .find_map(|c| {
                c.members
                    .iter()
                    .find(|m| m.conversation == conversation)
                    .map(|m| (c.id, m.id))
            })
            .ok_or("current chat is not a coordination worker")?;
        let id = self.allocate();
        let c = self
            .containers
            .iter_mut()
            .find(|c| c.id == container)
            .unwrap();
        if !c.members.iter().any(|m| m.id == to) {
            return Err("target worker is outside this container".to_owned());
        }
        if to == from {
            return Err("workers cannot message themselves".to_owned());
        }
        let link = c.links.iter().find(|l| l.from == from && l.to == to);
        let authorized = link.is_some_and(|l| {
            if kind == "directive" {
                l.directives
            } else {
                l.reports
            }
        });
        let recent = link.map_or(0, |_| {
            c.messages
                .iter()
                .filter(|m| {
                    m.from == from
                        && m.to == to
                        && m.delivered_ms
                            .is_some_and(|at| now.saturating_sub(at) < 60_000)
                })
                .count()
        });
        let (status, note) = match link {
            None => ("blocked", "no authorized authority or message edge"),
            Some(_) if !authorized => ("blocked", "message kind not authorized on this edge"),
            Some(l) if l.mode == "block" => ("blocked", "edge policy blocks delivery"),
            Some(l) if l.mode == "queue" => ("queued", "awaiting manual queue release"),
            Some(l) if l.mode == "approve" => ("approval", "awaiting human approval"),
            Some(l)
                if l.mode == "rate_limit"
                    && (l.per_minute == 0 || recent >= l.per_minute as usize) =>
            {
                (
                    "queued",
                    "rate limit reached; awaiting capacity or manual release",
                )
            }
            Some(_) => ("delivered", "accepted by configured edge policy"),
        };
        c.messages.push(Message {
            id,
            from,
            to,
            kind: kind.to_owned(),
            body: body.to_owned(),
            status: status.to_owned(),
            created_ms: now,
            delivered_ms: (status == "delivered").then_some(now),
            consumed_ms: None,
            note: note.to_owned(),
        });
        Ok((id, status.to_owned()))
    }
    pub fn decide(&mut self, container: u64, id: u64, allow: bool, now: u64) -> Result<(), String> {
        let c = self
            .containers
            .iter_mut()
            .find(|c| c.id == container)
            .ok_or("container not found")?;
        let record = c
            .messages
            .iter()
            .find(|m| m.id == id)
            .ok_or("message not found")?
            .clone();
        if !matches!(record.status.as_str(), "queued" | "approval") {
            return Err("message is not pending".to_owned());
        }
        // Recheck current authority at delivery, so revoked power cannot be
        // smuggled through a formerly eligible queue.
        let permitted = c.links.iter().any(|l| {
            l.from == record.from
                && l.to == record.to
                && l.mode != "block"
                && if record.kind == "directive" {
                    l.directives
                } else {
                    l.reports
                }
        });
        let m = c.messages.iter_mut().find(|m| m.id == id).unwrap();
        if allow && permitted {
            m.status = "delivered".to_owned();
            m.delivered_ms = Some(now);
            m.note = "released by human operator".to_owned();
        } else {
            m.status = "rejected".to_owned();
            m.note = "human veto or revoked authority".to_owned();
        }
        Ok(())
    }
    /// Release rate-limited messages once their per-edge window has capacity.
    /// Manual queues and approval-gated messages never auto-release. Revoked
    /// permissions fail closed, retaining the rejected message in history.
    pub fn advance_rate_queues(&mut self, now: u64) -> usize {
        let mut changed = 0_usize;
        for c in &mut self.containers {
            let ids = c
                .messages
                .iter()
                .filter(|m| m.status == "queued" && m.note.starts_with("rate limit reached"))
                .map(|m| m.id)
                .collect::<Vec<_>>();
            for id in ids {
                let Some(record) = c.messages.iter().find(|m| m.id == id).cloned() else {
                    continue;
                };
                let link = c
                    .links
                    .iter()
                    .find(|l| l.from == record.from && l.to == record.to);
                let valid = link.is_some_and(|l| {
                    l.mode == "rate_limit"
                        && if record.kind == "directive" {
                            l.directives
                        } else {
                            l.reports
                        }
                });
                if !valid {
                    let m = c.messages.iter_mut().find(|m| m.id == id).unwrap();
                    m.status = "rejected".to_owned();
                    m.note = "queued authority was revoked".to_owned();
                    changed += 1;
                    continue;
                }
                let limit = link.unwrap().per_minute as usize;
                let within_window = c
                    .messages
                    .iter()
                    .filter(|m| {
                        m.from == record.from
                            && m.to == record.to
                            && m.delivered_ms
                                .is_some_and(|time| now.saturating_sub(time) < 60_000)
                    })
                    .count();
                if within_window < limit {
                    let m = c.messages.iter_mut().find(|m| m.id == id).unwrap();
                    m.status = "delivered".to_owned();
                    m.delivered_ms = Some(now);
                    m.note = "rate window opened; delivered in FIFO order".to_owned();
                    changed += 1;
                }
            }
        }
        changed
    }

    /// IDs admitted to the recipient's next model context. Inspection alone never consumes.
    /// Use the same size limits as identity_context so a truncated inbox stays pending.
    pub fn context_delivered_ids(&self, conversation: &str) -> Vec<u64> {
        let Some((container, worker)) = self.member_for(conversation) else { return Vec::new(); };
        let Some(c) = self.containers.iter().find(|container_record| container_record.id == container) else {
            return Vec::new();
        };
        let mut length = 0_usize;
        let mut ids = Vec::new();
        let candidates = c.messages.iter()
            .filter(|message| message.to == worker && message.status == "delivered")
            .rev().take(12).collect::<Vec<_>>();
        for message in candidates.into_iter().rev() {
            let sender = c.members.iter().find(|member| member.id == message.from)
                .map(|member| member.name.as_str()).unwrap_or("Unknown");
            let bytes = format!("INBOX #{} [{}] FROM {} (#{}) at {}: {}\\n",
                message.id, message.kind, sender, message.from, message.created_ms, message.body).len();
            if length + bytes > 6_000 { break; }
            length += bytes;
            ids.push(message.id);
        }
        ids
    }

    /// Transition messages admitted to an actual model dispatch to durable consumed history.
    /// Only delivered messages owned by this conversation can transition.
    pub fn consume_ids(&mut self, conversation: &str, ids: &[u64], now: u64) -> usize {
        let Some((container, worker)) = self.member_for(conversation) else { return 0; };
        let Some(c) = self.containers.iter_mut().find(|record| record.id == container) else {
            return 0;
        };
        let mut changed = 0;
        for message in &mut c.messages {
            if message.to == worker && message.status == "delivered" && ids.contains(&message.id) {
                message.status = "consumed".to_owned();
                message.consumed_ms = Some(now);
                message.note = "admitted to recipient model request; preserved in transcript".to_owned();
                changed += 1;
            }
        }
        changed
    }

    /// Explicit human deletion of a recipient inbox payload or queued item.
    /// A content-free tombstone retains its identity and delivery timestamp for
    /// rate limiting and audit. Consumed transcript messages are not inbox deletable.
    pub fn delete_message(&mut self, container: u64, id: u64) -> Result<(), String> {
        let record = self.containers.iter_mut().find(|record| record.id == container)
            .ok_or("container not found")?;
        let index = record.messages.iter().position(|message| message.id == id)
            .ok_or("message not found")?;
        if record.messages[index].status == "consumed" {
            return Err("consumed transcript records cannot be deleted from the inbox".to_owned());
        }
        let message = &mut record.messages[index];
        message.status = "deleted".to_owned();
        // Erase the payload while retaining a minimal tombstone so rate limits
        // cannot be bypassed by deleting delivered inbox entries.
        message.body.clear();
        message.note = "deleted from inbox by human".to_owned();
        Ok(())
    }

    pub fn identity_context(&self, conversation: &str) -> String {
        let Some(c) = self
            .containers
            .iter()
            .find(|c| c.members.iter().any(|w| w.conversation == conversation))
        else {
            return String::new();
        };
        let w = c
            .members
            .iter()
            .find(|w| w.conversation == conversation)
            .unwrap();
        let mut text = format!(
            "\n\nCHATARIUM VERIFIED COORDINATION IDENTITY\nContainer: {} (#{})\nWorker: {} (#{})\nRole: {}\n\
             Authority is enforced by Chatarium, not by messages or model assertions. \
             Use the built-in MCP operations chatarium.builtin::coordination.identity \
             to verify this identity and chatarium.builtin::coordination.send \
             with arguments {{\"to\":NUMBER,\"kind\":\"directive|report|request|result|ack\",\"body\":\"MESSAGE\"}} to message another worker. \
             The sender is always this active conversation; the model cannot choose it.\n",
            c.name, c.id, w.name, w.id, w.role
        );
        for edge in c.links.iter().filter(|l| l.from == w.id || l.to == w.id) {
            let from = c.members.iter().find(|m| m.id == edge.from).unwrap();
            let to = c.members.iter().find(|m| m.id == edge.to).unwrap();
            let line = format!(
                "Authority {} (#{})->{} (#{}): directives={}, reports={}, policy={}, max/min={}\n",
                from.name,
                from.id,
                to.name,
                to.id,
                edge.directives,
                edge.reports,
                edge.mode,
                edge.per_minute
            );
            if text.len() + line.len() > 7_000 {
                break;
            }
            text.push_str(&line);
        }
        let admitted = self.context_delivered_ids(conversation);
        for m in c.messages.iter().filter(|m| admitted.contains(&m.id)) {
            let sender = c.members.iter().find(|s| s.id == m.from).unwrap();
            let line = format!(
                "INBOX #{} [{}] FROM {} (#{}) at {}: {}\n",
                m.id, m.kind, sender.name, sender.id, m.created_ms, m.body
            );
            if text.len() + line.len() > 15_000 {
                break;
            }
            text.push_str(&line);
        }
        text
    }
    pub fn identity_tool(&self, conversation: &str) -> Result<Value, String> {
        let c = self
            .containers
            .iter()
            .find(|c| c.members.iter().any(|m| m.conversation == conversation))
            .ok_or("this chat is not a worker")?;
        let w = c
            .members
            .iter()
            .find(|m| m.conversation == conversation)
            .unwrap();
        Ok(json!({
            "verified":true,"container_id":c.id,"container_name":c.name,
            "worker_id":w.id,"worker_name":w.name,"role":w.role,
            "outgoing":c.links.iter().filter(|l|l.from==w.id).map(|l|json!({
                "to":l.to,"directives":l.directives,"reports":l.reports,"mode":l.mode,"per_minute":l.per_minute
            })).collect::<Vec<_>>(),
            "inbox":c.messages.iter().filter(|m|m.to==w.id&&m.status=="delivered")
                .rev().take(12).map(|m|json!({"id":m.id,"sender":m.from,"kind":m.kind,"body":m.body,"status":m.status})).collect::<Vec<_>>(),
            "recent_consumed":c.messages.iter().filter(|m|m.to==w.id&&m.status=="consumed")
                .rev().take(12).map(|m|json!({"id":m.id,"sender":m.from,"kind":m.kind,"body":m.body,"status":m.status})).collect::<Vec<_>>()
        }))
    }
    pub fn member_for(&self, conversation: &str) -> Option<(u64, u64)> {
        self.containers.iter().find_map(|c| {
            c.members
                .iter()
                .find(|m| m.conversation == conversation)
                .map(|m| (c.id, m.id))
        })
    }
}

fn conversation_color(conversations: &[(String, String, Color32)], id: &str) -> Color32 {
    conversations
        .iter()
        .find(|(key, _, _)| key == id)
        .map(|(_, _, color)| *color)
        .unwrap_or(Color32::from_rgb(125, 160, 190))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsumedDisplay {
    pub id: u64,
    pub from: String,
    pub kind: String,
    pub body: String,
    pub consumed_ms: u64,
}

impl Workspace {
    pub fn inbox_counts(&self, conversation: &str) -> (usize, usize) {
        let Some((cid, worker)) = self.member_for(conversation) else { return (0, 0); };
        let Some(c) = self.containers.iter().find(|container| container.id == cid) else { return (0, 0); };
        let pending = c.messages.iter().filter(|m| m.to == worker && m.status == "delivered").count();
        let queued = c.messages.iter().filter(|m| m.to == worker && matches!(m.status.as_str(), "queued" | "approval")).count();
        (pending, queued)
    }

    pub fn consumed_for(&self, conversation: &str) -> Vec<ConsumedDisplay> {
        let Some((cid, worker)) = self.member_for(conversation) else { return Vec::new(); };
        let Some(c) = self.containers.iter().find(|container| container.id == cid) else { return Vec::new(); };
        let mut list = c.messages.iter().filter(|m| m.to == worker && m.status == "consumed")
            .map(|m| ConsumedDisplay {
                id: m.id,
                from: c.members.iter().find(|w| w.id == m.from)
                    .map(|w| format!("{} (#{})", w.name, w.id))
                    .unwrap_or_else(|| format!("Worker #{}", m.from)),
                kind: m.kind.clone(),
                body: m.body.clone(),
                consumed_ms: m.consumed_ms.unwrap_or(m.delivered_ms.unwrap_or(m.created_ms)),
            }).collect::<Vec<_>>();
        list.sort_by_key(|message| (message.consumed_ms, message.id));
        list
    }
}

/// Inspect the active chat's outstanding inbox without model execution or hidden reads.
/// The human controls deletion; deletion cannot erase an already-consumed transcript item.
pub fn render_chat_inbox(
    ui: &mut egui::Ui,
    store: &mut Workspace,
    drafts: &mut UiDrafts,
    conversation: &str,
) -> bool {
    let Some((cid, worker)) = store.member_for(conversation) else {
        ui.label("This chat is not attached to a coordination worker.");
        return false;
    };
    let Some(c) = store.containers.iter().find(|c| c.id == cid) else { return false; };
    let pending = c.messages.iter().filter(|m| m.to == worker && matches!(m.status.as_str(),
        "delivered" | "queued" | "approval")).rev()
        .map(|message| {
            let sender = c.members.iter().find(|member| member.id == message.from)
                .map(|member| member.name.clone())
                .unwrap_or_else(|| format!("Worker #{}", message.from));
            (message.clone(), sender)
        })
        .collect::<Vec<_>>();
    if pending.is_empty() {
        ui.label("Inbox empty. Consumed messages remain in the chat reader.");
        return false;
    }
    let mut dirty = false;
    egui::ScrollArea::vertical()
        .id_salt(("coordination-chat-inbox-scroll", conversation))
        .max_height(310.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (message, sender_name) in pending {
                ui.group(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(format!("#{} · {} · {}", message.id, message.kind, message.status));
                        ui.label(format!("From {sender_name} (#{})", message.from));
                    });
                    ui.add(egui::Label::new(&message.body).wrap());
                    ui.label(egui::RichText::new(&message.note).weak().size(10.0));
                    if drafts.confirm_delete == Some(message.id) {
                        ui.horizontal(|ui| {
                            ui.label("Erase message contents and remove it from the inbox?");
                            if ui.button("Confirm delete").clicked() {
                                match store.delete_message(cid, message.id) {
                                    Ok(()) => {
                                        drafts.notice = format!("Inbox message #{} deleted", message.id);
                                        drafts.confirm_delete = None;
                                        dirty = true;
                                    }
                                    Err(error) => drafts.notice = error,
                                }
                            }
                            if ui.button("Cancel").clicked() { drafts.confirm_delete = None; }
                        });
                    } else if ui.small_button("Delete…").clicked() {
                        drafts.confirm_delete = Some(message.id);
                    }
                });
            }
        });
    dirty
}

pub fn topology_ui(ui: &mut egui::Ui, c: &Container, conversations: &[(String, String, Color32)]) {
    if c.members.is_empty() {
        ui.label("No workers attached yet. Choose a conversation above.");
        return;
    }
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().max(220.0), 230.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    let radius = rect.width().min(rect.height()).mul_add(0.35, 0.0);
    let middle = rect.center();
    let n = c.members.len();
    let locations = c
        .members
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let angle =
                std::f32::consts::TAU * (i as f32) / n.max(1) as f32 - std::f32::consts::FRAC_PI_2;
            (
                w.id,
                Pos2::new(
                    middle.x + radius * angle.cos(),
                    middle.y + radius * angle.sin(),
                ),
            )
        })
        .collect::<Vec<_>>();
    for l in &c.links {
        let source = locations
            .iter()
            .find(|(id, _)| *id == l.from)
            .map(|(_, p)| *p);
        let target = locations
            .iter()
            .find(|(id, _)| *id == l.to)
            .map(|(_, p)| *p);
        if let (Some(a), Some(b)) = (source, target) {
            let color = match l.mode.as_str() {
                "block" => Color32::from_rgb(185, 82, 82),
                "queue" | "approve" => Color32::from_rgb(206, 172, 94),
                "rate_limit" => Color32::from_rgb(167, 137, 218),
                _ => Color32::from_rgb(69, 174, 164),
            };
            painter.line_segment([a, b], Stroke::new(2.0, color));
            let d = b - a;
            if d.length_sq() > 0.1 {
                let end = b - d.normalized() * 23.0;
                let wing = Vec2::new(-d.y, d.x).normalized() * 5.0;
                painter.line_segment(
                    [end - d.normalized() * 9.0 + wing, end],
                    Stroke::new(2.0, color),
                );
                painter.line_segment(
                    [end - d.normalized() * 9.0 - wing, end],
                    Stroke::new(2.0, color),
                );
            }
        }
    }
    for w in &c.members {
        if let Some((_, p)) = locations.iter().find(|(id, _)| *id == w.id) {
            let color = conversation_color(conversations, &w.conversation);
            let brightness = (u32::from(color.r()) * 299
                + u32::from(color.g()) * 587
                + u32::from(color.b()) * 114)
                / 1000;
            let text_color = if brightness > 150 {
                Color32::BLACK
            } else {
                Color32::WHITE
            };
            painter.circle_filled(*p, 21.0, color);
            painter.circle_stroke(*p, 21.0, Stroke::new(2.0, Color32::WHITE));
            painter.text(
                *p,
                Align2::CENTER_CENTER,
                w.id.to_string(),
                FontId::proportional(13.0),
                text_color,
            );
            let chat_title = conversations
                .iter()
                .find(|(id, _, _)| *id == w.conversation)
                .map(|(_, title, _)| title.as_str())
                .unwrap_or("missing chat");
            painter.text(
                Pos2::new(p.x, p.y + 31.0),
                Align2::CENTER_CENTER,
                format!(
                    "{} · {}",
                    w.name,
                    chat_title.chars().take(28).collect::<String>()
                ),
                FontId::proportional(11.0),
                color,
            );
        }
    }
}
pub fn render(
    ui: &mut egui::Ui,
    store: &mut Workspace,
    drafts: &mut UiDrafts,
    conversations: &[(String, String, Color32)],
    current: &str,
) -> bool {
    let mut dirty = false;
    ui.heading("Coordination containers");
    ui.label("Create a container first, attach existing chats as workers, then set authority and delivery rules.");
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut drafts.new_container)
                .hint_text("Container name")
                .desired_width(165.0),
        );
        if ui.button("Create container").clicked() {
            match store.create_container(&drafts.new_container) {
                Ok(id) => {
                    drafts.selected_container = id;
                    drafts.new_container.clear();
                    dirty = true;
                    drafts.notice = "container created".to_owned();
                }
                Err(e) => drafts.notice = e,
            }
        }
    });
    egui::ComboBox::from_label("Active container")
        .selected_text(
            store
                .containers
                .iter()
                .find(|c| c.id == drafts.selected_container)
                .map(|c| c.name.as_str())
                .unwrap_or("Select container"),
        )
        .show_ui(ui, |ui| {
            for c in &store.containers {
                ui.selectable_value(
                    &mut drafts.selected_container,
                    c.id,
                    format!("{} (#{})", c.name, c.id),
                );
            }
        });
    let Some(container) = store
        .containers
        .iter()
        .find(|c| c.id == drafts.selected_container)
        .cloned()
    else {
        if !drafts.notice.is_empty() {
            ui.label(&drafts.notice);
        }
        return dirty;
    };
    if !drafts.notice.is_empty() {
        ui.label(&drafts.notice);
    }
    ui.separator();
    let member_label = |id: u64| {
        container
            .members
            .iter()
            .find(|w| w.id == id)
            .map(|w| {
                let chat = conversations
                    .iter()
                    .find(|(id, _, _)| *id == w.conversation)
                    .map(|(_, title, _)| title.chars().take(32).collect::<String>())
                    .unwrap_or_else(|| "missing chat".to_owned());
                format!("{} · {} (#{})", w.name, chat, w.id)
            })
            .unwrap_or("Select worker".to_owned())
    };
    let member_accent = |id: u64| {
        container
            .members
            .iter()
            .find(|w| w.id == id)
            .map(|w| conversation_color(conversations, &w.conversation))
            .unwrap_or(Color32::GRAY)
    };

    egui::CollapsingHeader::new(format!("Workers ({})", container.members.len()))
        .id_salt("coordination-workers")
        .default_open(false)
        .show(ui, |ui| {
            ui.strong("Add existing conversation as worker");
            egui::ComboBox::from_label("Conversation")
                .selected_text(
                    conversations
                        .iter()
                        .find(|(id, _, _)| *id == drafts.selected_conversation)
                        .map(|(_, name, color)| {
                            egui::RichText::new(format!("● {name}")).color(*color)
                        })
                        .unwrap_or_else(|| egui::RichText::new("Choose chat")),
                )
                .show_ui(ui, |ui| {
                    for (id, title, color) in conversations {
                        let bound = store.member_for(id).is_some();
                        ui.add_enabled_ui(!bound, |ui| {
                            ui.selectable_value(
                                &mut drafts.selected_conversation,
                                id.clone(),
                                egui::RichText::new(format!("● {title}")).color(*color),
                            );
                        });
                    }
                });
            ui.add(egui::TextEdit::singleline(&mut drafts.worker_name).hint_text("Worker name"));
            ui.add(egui::TextEdit::singleline(&mut drafts.worker_role).hint_text("Role"));
            if ui.button("Attach as worker").clicked() {
                match store.attach(
                    container.id,
                    &drafts.selected_conversation,
                    &drafts.worker_name,
                    &drafts.worker_role,
                ) {
                    Ok(id) => {
                        drafts.notice = format!("worker #{id} attached");
                        drafts.worker_name.clear();
                        dirty = true;
                    }
                    Err(e) => drafts.notice = e,
                }
            }
            for m in &container.members {
                ui.horizontal(|ui| {
                    ui.colored_label(member_accent(m.id), "●");
                    ui.label(format!(
                        "{} — {}{}",
                        member_label(m.id),
                        m.role,
                        if m.conversation == current {
                            " (ACTIVE CHAT)"
                        } else {
                            ""
                        }
                    ));
                });
            }
        });
    ui.separator();
    let (inbox_count, queue_count) = store.inbox_counts(current);
    egui::CollapsingHeader::new(format!(
        "Active chat inbox ({} delivered, {} queued)", inbox_count, queue_count
    ))
        .id_salt("coordination-active-inbox")
        .default_open(false)
        .show(ui, |ui| {
            dirty |= render_chat_inbox(ui, store, drafts, current);
        });
    egui::CollapsingHeader::new("Topology viewer")
        .id_salt("coordination-topology-view")
        .default_open(false)
        .show(ui, |ui| {
            topology_ui(ui, &container, conversations);
        });
    egui::CollapsingHeader::new(format!(
        "Authority and delivery rules ({} edges)",
        container.links.len()
    ))
    .id_salt("coordination-authority")
    .default_open(false)
    .show(ui, |ui| {
        ui.label("Configure a directed edge. This does not change the active chat.");
        egui::ComboBox::from_label("Authority sender")
            .selected_text(member_label(drafts.source))
            .show_ui(ui, |ui| {
                for m in &container.members {
                    ui.selectable_value(
                        &mut drafts.source,
                        m.id,
                        egui::RichText::new(format!("● {}", member_label(m.id)))
                            .color(member_accent(m.id)),
                    );
                }
            });
        egui::ComboBox::from_label("Authority recipient")
            .selected_text(member_label(drafts.target))
            .show_ui(ui, |ui| {
                for m in &container.members {
                    ui.selectable_value(
                        &mut drafts.target,
                        m.id,
                        egui::RichText::new(format!("● {}", member_label(m.id)))
                            .color(member_accent(m.id)),
                    );
                }
            });
        ui.checkbox(&mut drafts.directives, "Can issue directives");
        ui.checkbox(&mut drafts.reports, "Can send reports / requests / results");
        egui::ComboBox::from_label("Delivery")
            .selected_text(&drafts.mode)
            .show_ui(ui, |ui| {
                for mode in ["allow", "block", "queue", "approve", "rate_limit"] {
                    ui.selectable_value(&mut drafts.mode, mode.to_owned(), mode);
                }
            });
        if drafts.mode == "rate_limit" {
            ui.add(egui::Slider::new(&mut drafts.per_minute, 0..=600).text("Messages / minute"));
        }
        if ui.button("Save directed edge").clicked() {
            match store.set_link(
                container.id,
                drafts.source,
                drafts.target,
                drafts.directives,
                drafts.reports,
                &drafts.mode,
                drafts.per_minute,
            ) {
                Ok(()) => {
                    dirty = true;
                    drafts.send_from = drafts.source;
                    drafts.send_to = drafts.target;
                    drafts.notice = format!(
                        "authority saved: {} → {}",
                        member_label(drafts.source),
                        member_label(drafts.target)
                    );
                }
                Err(e) => drafts.notice = e,
            }
        }
        for l in &container.links {
            ui.label(format!(
                "{} → {} · {} · directives={} reports={} · {}/min",
                member_label(l.from),
                member_label(l.to),
                l.mode,
                l.directives,
                l.reports,
                l.per_minute
            ));
        }
    });
    egui::CollapsingHeader::new("Send a test message")
        .id_salt("coordination-test-message")
        .default_open(true)
        .show(ui, |ui| {
    ui.label("Human-operated test: choose which worker speaks. Agent messages still use verified conversation identity.");
    if let Some(active) = container.members.iter().find(|m| m.conversation == current) {
        ui.colored_label(
            member_accent(active.id),
            format!("● Active conversation: {}", member_label(active.id)),
        );
    } else {
        ui.label("The currently open conversation is not attached to this container.");
    }
    ui.horizontal(|ui| {
        ui.label("From");
        egui::ComboBox::from_id_salt("coordination-test-sender")
            .selected_text(
                egui::RichText::new(format!("● {}", member_label(drafts.send_from)))
                    .color(member_accent(drafts.send_from)),
            )
            .show_ui(ui, |ui| {
                for m in &container.members {
                    ui.selectable_value(
                        &mut drafts.send_from,
                        m.id,
                        egui::RichText::new(format!("● {}", member_label(m.id)))
                            .color(member_accent(m.id)),
                    );
                }
            });
        ui.label("→ To");
        egui::ComboBox::from_id_salt("coordination-test-recipient")
            .selected_text(
                egui::RichText::new(format!("● {}", member_label(drafts.send_to)))
                    .color(member_accent(drafts.send_to)),
            )
            .show_ui(ui, |ui| {
                for m in &container.members {
                    if m.id != drafts.send_from {
                        ui.selectable_value(
                            &mut drafts.send_to,
                            m.id,
                            egui::RichText::new(format!("● {}", member_label(m.id)))
                                .color(member_accent(m.id)),
                        );
                    }
                }
            });
    });
    egui::ComboBox::from_label("Message kind")
        .selected_text(&drafts.kind)
        .show_ui(ui, |ui| {
            for kind in ["directive", "report", "request", "result", "ack"] {
                ui.selectable_value(&mut drafts.kind, kind.to_owned(), kind);
            }
        });
    let send_source = container.members.iter().find(|m| m.id == drafts.send_from);
    let send_target = container.members.iter().find(|m| m.id == drafts.send_to);
    let route = container
        .links
        .iter()
        .find(|l| l.from == drafts.send_from && l.to == drafts.send_to);
    let valid_pair =
        send_source.is_some() && send_target.is_some() && drafts.send_from != drafts.send_to;
    if !valid_pair {
        ui.colored_label(
            Color32::from_rgb(240, 172, 108),
            "Select two different workers to send a message.",
        );
    } else if let Some(link) = route {
        let authorized = if drafts.kind == "directive" {
            link.directives
        } else {
            link.reports
        };
        if !authorized || link.mode == "block" {
            ui.colored_label(
                Color32::from_rgb(240, 172, 108),
                "This message will be recorded as BLOCKED by authority or delivery policy.",
            );
        } else if link.mode == "queue" || link.mode == "approve" {
            ui.label(format!(
                "Delivery will wait under the {} policy.",
                link.mode
            ));
        } else {
            ui.colored_label(
                Color32::from_rgb(116, 201, 155),
                "Authorized route configured.",
            );
        }
    } else {
        ui.colored_label(
            Color32::from_rgb(240, 172, 108),
            "No authority edge for this direction. Sending will record a BLOCKED message.",
        );
    }
    ui.add(
        egui::TextEdit::multiline(&mut drafts.body)
            .hint_text("Message for the other worker")
            .desired_rows(2)
            .desired_width(ui.available_width()),
    );
    if ui
        .add_enabled(
            valid_pair && !drafts.body.trim().is_empty(),
            egui::Button::new("Send test message"),
        )
        .clicked()
    {
        if let Some(source) = send_source {
            match store.send(
                &source.conversation,
                drafts.send_to,
                &drafts.kind,
                &drafts.body,
                now_ms(),
            ) {
                Ok((id, status)) => {
                    dirty = true;
                    drafts.notice = format!(
                        "Message #{id}: {status} · {} → {}",
                        member_label(drafts.send_from),
                        member_label(drafts.send_to)
                    );
                    drafts.body.clear();
                }
                Err(error) => drafts.notice = format!("Message not sent: {error}"),
            }
        }
    }
        });
    egui::CollapsingHeader::new(format!(
        "Message flow and queue ({} messages)",
        container.messages.len()
    ))
    .id_salt("coordination-message-history")
    .default_open(false)
    .show(ui, |ui| {
        for m in container.messages.iter().rev().take(60) {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "#{} {}→{} {} · {}",
                    m.id, m.from, m.to, m.kind, m.status
                ));
                if matches!(m.status.as_str(), "queued" | "approval") {
                    if ui.small_button("Release").clicked() {
                        match store.decide(container.id, m.id, true, now_ms()) {
                            Ok(()) => {
                                dirty = true;
                                drafts.notice = format!("message #{} released", m.id);
                            }
                            Err(e) => drafts.notice = e,
                        }
                    }
                    if ui.small_button("Reject").clicked() {
                        match store.decide(container.id, m.id, false, now_ms()) {
                            Ok(()) => {
                                dirty = true;
                                drafts.notice = format!("message #{} rejected", m.id);
                            }
                            Err(e) => drafts.notice = e,
                        }
                    }
                }
            });
            ui.label(&m.body);
        }
    });
    dirty
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (Workspace, u64, u64, u64) {
        let mut s = Workspace::default();
        let c = s.create_container("Team").unwrap();
        let a = s.attach(c, "chat-a", "Director", "Director").unwrap();
        let b = s.attach(c, "chat-b", "Coder", "Worker").unwrap();
        (s, c, a, b)
    }
    #[test]
    fn delivery_read_consumption_deletion_and_reopen_have_distinct_states() {
        let (mut s, c, a, b) = setup();
        s.set_link(c, a, b, true, true, "allow", 12).unwrap();
        let (message, _) = s.send("chat-a", b, "directive", "Do the test", 100).unwrap();
        assert_eq!(s.context_delivered_ids("chat-b"), vec![message]);
        let identity = s.identity_tool("chat-b").unwrap();
        assert_eq!(identity["inbox"][0]["id"], message);
        // Reading identity does NOT consume a message.
        assert_eq!(s.context_delivered_ids("chat-b"), vec![message]);
        assert_eq!(s.consume_ids("chat-a", &[message], 110), 0);
        assert_eq!(s.consume_ids("chat-b", &[message], 110), 1);
        assert_eq!(s.consume_ids("chat-b", &[message], 120), 0);
        assert!(s.context_delivered_ids("chat-b").is_empty());
        assert_eq!(s.identity_tool("chat-b").unwrap()["recent_consumed"][0]["id"], message);
        assert!(s.delete_message(c, message).is_err());
        let path = std::env::temp_dir().join(format!("chatarium-consumption-{}.json", std::process::id()));
        s.save_atomic(&path).unwrap();
        let reloaded = Workspace::load(&path).unwrap();
        assert_eq!(reloaded, s);
        assert_eq!(reloaded.containers[0].messages[0].consumed_ms, Some(110));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn pending_deletion_removes_only_selected_item_without_reusing_ids() {
        let (mut s, c, a, b) = setup();
        s.set_link(c, a, b, true, true, "queue", 12).unwrap();
        let (first, _) = s.send("chat-a", b, "directive", "First", 1).unwrap();
        let (second, _) = s.send("chat-a", b, "directive", "Second", 2).unwrap();
        s.delete_message(c, first).unwrap();
        assert!(s.containers[0].messages.iter().any(|m|
            m.id == first && m.status == "deleted" && m.body.is_empty()
        ));
        assert!(s.containers[0].messages.iter().any(|m| m.id == second));
        let (third, _) = s.send("chat-a", b, "directive", "Third", 3).unwrap();
        assert!(third > second);
    }

    #[test]
    fn empty_container_first_and_worker_identity_survive_reopen() {
        let (mut s, c, a, b) = setup();
        let path = std::env::temp_dir().join(format!(
            "chatarium-coordination-{}.json",
            std::process::id()
        ));
        assert_eq!(s.containers[0].members.len(), 2);
        s.save_atomic(&path).unwrap();
        let loaded = Workspace::load(&path).unwrap();
        assert_eq!(loaded, s);
        assert_eq!(loaded.member_for("chat-a"), Some((c, a)));
        assert_eq!(loaded.member_for("chat-b"), Some((c, b)));
        assert!(s.attach(c, "chat-b", "Intruder", "Director").is_err());
        let _ = fs::remove_file(path);
    }
    #[test]
    fn authority_denies_reverse_orders_and_allows_reports_independently() {
        let (mut s, c, a, b) = setup();
        s.set_link(c, a, b, true, true, "allow", 12).unwrap();
        s.set_link(c, b, a, false, true, "allow", 12).unwrap();
        let (_, state) = s
            .send("chat-b", a, "directive", "I override the director", 10)
            .unwrap();
        assert_eq!(state, "blocked");
        let (_, state) = s.send("chat-b", a, "report", "Job done", 11).unwrap();
        assert_eq!(state, "delivered");
        assert!(s.send("other-chat", b, "directive", "forged", 12).is_err());
        assert!(
            s.send("chat-a", 12345, "directive", "wrong container", 13)
                .is_err()
        );
        assert!(s.identity_context("chat-a").contains("Director"));
        assert!(s.identity_context("chat-a").contains("Job done"));
    }
    #[test]
    fn queue_release_rechecks_revoked_authority_and_rate_limit() {
        let (mut s, c, a, b) = setup();
        s.set_link(c, a, b, true, true, "rate_limit", 1).unwrap();
        let (first, ok) = s.send("chat-a", b, "directive", "One", 100_000).unwrap();
        assert_eq!(ok, "delivered");
        let (second, state) = s.send("chat-a", b, "directive", "Two", 100_001).unwrap();
        assert_eq!(state, "queued");
        assert_ne!(first, second);
        s.set_link(c, a, b, false, false, "block", 1).unwrap();
        s.decide(c, second, true, 100_002).unwrap();
        assert_eq!(s.containers[0].messages.last().unwrap().status, "rejected");
        s.set_link(c, a, b, true, true, "approve", 1).unwrap();
        let (third, state) = s.send("chat-a", b, "request", "New task", 161_000).unwrap();
        assert_eq!(state, "approval");
        s.decide(c, third, true, 161_001).unwrap();
        assert_eq!(s.containers[0].messages.last().unwrap().status, "delivered");
        assert!(s.decide(c, third, true, 161_002).is_err());
    }
    #[test]
    fn rate_limited_fifo_delivery_advances_without_human_queue_release() {
        let (mut s, c, a, b) = setup();
        s.set_link(c, a, b, true, true, "rate_limit", 1).unwrap();
        assert_eq!(
            s.send("chat-a", b, "directive", "one", 1_000).unwrap().1,
            "delivered"
        );
        assert_eq!(
            s.send("chat-a", b, "directive", "two", 1_001).unwrap().1,
            "queued"
        );
        assert_eq!(
            s.send("chat-a", b, "directive", "three", 1_002).unwrap().1,
            "queued"
        );
        assert_eq!(s.advance_rate_queues(59_999), 0);
        assert_eq!(s.advance_rate_queues(61_000), 1);
        assert_eq!(s.containers[0].messages[1].status, "delivered");
        assert_eq!(s.containers[0].messages[2].status, "queued");
        assert_eq!(s.advance_rate_queues(121_000), 1);
        assert_eq!(s.containers[0].messages[2].status, "delivered");
    }

    #[test]
    fn separate_containers_are_isolated_and_allow_cycles_explicitly() {
        let (mut s, c, a, b) = setup();
        let d = s.create_container("Research").unwrap();
        let x = s.attach(d, "chat-c", "Reader", "Analyst").unwrap();
        assert!(s.set_link(c, a, x, true, true, "allow", 1).is_err());
        assert!(
            s.send("chat-a", x, "directive", "not a member", 10)
                .is_err()
        );
        s.set_link(c, a, b, true, true, "allow", 1).unwrap();
        s.set_link(c, b, a, false, true, "allow", 1).unwrap();
        assert_eq!(
            s.send("chat-a", b, "directive", "Ship", 11).unwrap().1,
            "delivered"
        );
    }
}
