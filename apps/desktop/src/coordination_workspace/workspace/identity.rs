//! identity methods for human-controlled coordination containers.
use super::super::*;

impl Workspace {
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
