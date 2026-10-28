//! topology methods for human-controlled coordination containers.
use super::super::*;

impl Workspace {
    pub(super) fn allocate(&mut self) -> u64 {
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
    /// An authorized directive edge implies a report-only return route.
    /// Explicit reverse policies, including blocks, take precedence.
    pub(in crate::coordination_workspace) fn route_for(
        c: &Container,
        from: u64,
        to: u64,
    ) -> Option<(Link, bool)> {
        if let Some(edge) = c.links.iter().find(|l| l.from == from && l.to == to) {
            return Some((edge.clone(), false));
        }
        c.links
            .iter()
            .find(|l| l.from == to && l.to == from && l.directives && l.mode != "block")
            .map(|_| {
                (
                    Link {
                        from,
                        to,
                        directives: false,
                        reports: true,
                        mode: "allow".to_owned(),
                        per_minute: 0,
                    },
                    true,
                )
            })
    }
}
