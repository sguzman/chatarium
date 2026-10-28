//! Behavior-preserving extraction of catalog methods from the desktop application.
use super::super::super::*;

impl ChatariumApp {
    pub(crate) fn persist_local_conversation_catalog(&mut self) -> bool {
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot persist local conversation workspace: persistence unavailable".to_owned();
            return false;
        };
        if let Err(error) = sender.send(PersistCommand::SaveLocalConversationCatalog {
            catalog: self.local_conversation_catalog.clone(),
        }) {
            self.status = format!("failed to queue local conversation workspace: {error}");
            return false;
        }
        true
    }

    pub(crate) fn reader_conversation_key(&self) -> String {
        self.selected_historical_conversation
            .map(|conversation| format!("mirror:{conversation}"))
            .unwrap_or_else(|| format!("local:{}", self.local_conversation_id))
    }
}
