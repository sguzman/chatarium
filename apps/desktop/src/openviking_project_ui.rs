//! Native opt-in project checkpoint sharing controls and durable action.
//! The same egui widget path is exercised by pointer-event tests.
use crate::local_conversations::LocalConversationCatalog;
use crate::openviking_memory_delivery::ProjectMemoryGrants;
use chatarium_core::LocalConversationId;
use eframe::egui;
use std::path::Path;

/// Draw controls for the selected destination. Folder equality merely makes
/// candidates visible; it NEVER grants permission on its own.
pub fn render(
    ui: &mut egui::Ui,
    catalog: &LocalConversationCatalog,
    grants: &ProjectMemoryGrants,
    destination: LocalConversationId,
) -> Option<(LocalConversationId, bool)> {
    render_with_button(ui, catalog, grants, destination, |ui, label| {
        ui.button(label).clicked()
    })
}

fn render_with_button(
    ui: &mut egui::Ui,
    catalog: &LocalConversationCatalog,
    grants: &ProjectMemoryGrants,
    destination: LocalConversationId,
    mut clicked: impl FnMut(&mut egui::Ui, &str) -> bool,
) -> Option<(LocalConversationId, bool)> {
    ui.label("Only manually approved source chats can share verified checkpoints into this chat. Sharing is one-way and reversible.");
    let Some(project) = catalog
        .entry(destination)
        .and_then(|entry| entry.project_directory.as_deref())
    else {
        ui.label("Assign and save a project folder for this chat to manage sharing.");
        return None;
    };
    let peers = catalog
        .entries()
        .into_iter()
        .filter(|peer| {
            peer.id != destination
                && peer.project_directory.as_deref() == Some(project)
        })
        .collect::<Vec<_>>();
    if peers.is_empty() {
        ui.label("No other chat is bound to this project folder.");
        return None;
    }
    let mut request = None;
    for peer in peers {
        let allowed = grants.allows(catalog, peer.id, destination);
        ui.horizontal_wrapped(|ui| {
            ui.label(peer.title.as_deref().unwrap_or("Untitled chat"));
            let button_label = if allowed {
                "Revoke memory access"
            } else {
                "Allow memory from chat"
            };
            if clicked(ui, button_label) {
                request = Some((peer.id, !allowed));
            }
        });
    }
    request
}

/// Validate and persist before the in-memory permission change is admitted.
/// Failed persistence restores the prior permission snapshot.
pub fn change_permission_durable(
    path: &Path,
    catalog: &LocalConversationCatalog,
    grants: &mut ProjectMemoryGrants,
    source: LocalConversationId,
    destination: LocalConversationId,
    allow: bool,
) -> Result<(), String> {
    let previous = grants.clone();
    if allow {
        grants.grant(catalog, source, destination)?;
    } else {
        grants.revoke(source, destination);
    }
    if let Err(error) = grants.save_atomic(path) {
        *grants = previous;
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2};
    use std::cell::Cell;

    fn temp_root(prefix: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "chatarium-project-ui-{prefix}-{}-{nonce}", std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn fixture(
        root: &Path,
    ) -> (
        LocalConversationCatalog,
        LocalConversationId,
        LocalConversationId,
        LocalConversationId,
    ) {
        let source = LocalConversationId::new();
        let destination = LocalConversationId::new();
        let unrelated = LocalConversationId::new();
        let shared = root.join("shared");
        let foreign = root.join("foreign");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::create_dir_all(&foreign).unwrap();
        let mut catalog = LocalConversationCatalog::default();
        for (id, title, folder, time) in [
            (source, "Source chat", &shared, 10),
            (destination, "Destination chat", &shared, 20),
            (unrelated, "Foreign chat", &foreign, 30),
        ] {
            catalog.create(id, time);
            catalog.rename(id, Some(title.to_owned()), time + 1).unwrap();
            catalog
                .set_project_directory(id, Some(folder.display().to_string()), time + 2)
                .unwrap();
        }
        (catalog, source, destination, unrelated)
    }

    fn input(events: Vec<Event>) -> RawInput {
        RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 700.0))),
            events,
            ..Default::default()
        }
    }

    /// Simulates actual egui pointer press/release over the real button
    /// response instead of directly manufacturing a grant request.
    fn click_button(
        context: &egui::Context,
        catalog: &LocalConversationCatalog,
        grants: &ProjectMemoryGrants,
        destination: LocalConversationId,
        label: &str,
    ) -> Option<(LocalConversationId, bool)> {
        let bounds = Cell::new(None);
        let _ = context.run(input(Vec::new()), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let _ = render_with_button(
                    ui, catalog, grants, destination, |ui, text| {
                        let response = ui.button(text);
                        if text == label {
                            bounds.set(Some(response.rect));
                        }
                        response.clicked()
                    },
                );
            });
        });
        let pos = bounds.get().expect("target button must be rendered").center();
        let event = |pressed| {
            vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::default(),
                },
            ]
        };
        let mut pressed = None;
        let _ = context.run(input(event(true)), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                pressed = render(ui, catalog, grants, destination);
            });
        });
        let mut released = None;
        let _ = context.run(input(event(false)), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                released = render(ui, catalog, grants, destination);
            });
        });
        released.or(pressed)
    }

    #[test]
    fn real_egui_click_grants_persists_reloads_and_revokes() {
        let root = temp_root("click");
        let (catalog, source, destination, unrelated) = fixture(&root);
        let path = root.join("project-grants.json");
        let context = egui::Context::default();
        let mut grants = ProjectMemoryGrants::default();
        assert!(!grants.allows(&catalog, source, destination));
        let (selected, allowed) = click_button(
            &context, &catalog, &grants, destination, "Allow memory from chat",
        ).expect("real egui allow button must click");
        assert_eq!((selected, allowed), (source, true));
        change_permission_durable(&path, &catalog, &mut grants, selected, destination, allowed)
            .unwrap();
        let mut grants = ProjectMemoryGrants::load(&path).unwrap();
        assert!(grants.allows(&catalog, source, destination));
        assert!(!grants.allows(&catalog, destination, source));
        assert!(!grants.allows(&catalog, unrelated, destination));

        let (selected, allowed) = click_button(
            &context, &catalog, &grants, destination, "Revoke memory access",
        ).expect("real egui revoke button must click");
        assert_eq!((selected, allowed), (source, false));
        change_permission_durable(&path, &catalog, &mut grants, selected, destination, allowed)
            .unwrap();
        let reloaded = ProjectMemoryGrants::load(&path).unwrap();
        assert!(!reloaded.allows(&catalog, source, destination));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn foreign_project_cannot_be_selected_or_granted() {
        let root = temp_root("foreign");
        let (catalog, source, destination, unrelated) = fixture(&root);
        let mut grants = ProjectMemoryGrants::default();
        let path = root.join("project-grants.json");
        assert!(change_permission_durable(
            &path, &catalog, &mut grants, unrelated, destination, true,
        ).is_err());
        assert!(!path.exists());
        let mut candidates = Vec::new();
        let context = egui::Context::default();
        let _ = context.run(input(Vec::new()), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let _ = render_with_button(
                    ui, &catalog, &grants, destination, |ui, label| {
                        candidates.push(label.to_owned());
                        ui.button(label).clicked()
                    },
                );
            });
        });
        assert_eq!(candidates, vec!["Allow memory from chat"]);
        assert!(!grants.allows(&catalog, source, destination));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_permission_file_write_does_not_admit_in_memory_access() {
        let root = temp_root("rollback");
        let (catalog, source, destination, _) = fixture(&root);
        let mut grants = ProjectMemoryGrants::default();
        // A parent file cannot hold a child permission file.
        let blocker = root.join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();
        let path = blocker.join("grant.json");
        assert!(change_permission_durable(
            &path, &catalog, &mut grants, source, destination, true,
        ).is_err());
        assert!(!grants.allows(&catalog, source, destination));
        std::fs::remove_dir_all(root).unwrap();
    }
}
