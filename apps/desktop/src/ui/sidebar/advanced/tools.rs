//! tools controls extracted from the advanced sidebar.
//! Same closures and action order; no change to egui widget salts.
use super::*;

impl ChatariumApp {
    pub(super) fn render_mcp_tools(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.collapsing("Tools / MCP · restricted local adapter", |ui| {
            ui.label(
                egui::RichText::new(
                    "MCP agent calls run through exact saved tool policies and durable execution audits. Built-in read-only discovery and hello can run automatically. External providers require registration and Linux sandbox activation; unsupported host, browser, filesystem and network access remain disabled.",
                )
                .size(9.0)
                .color(egui::Color32::from_rgb(139, 143, 153)),
            );

            ui.horizontal_wrapped(|ui| {
                if ui.checkbox(&mut self.conversation_mcp_enabled, "Enable MCP tools in this conversation").changed() {
                    self.inference_settings.mark_mcp_activation_explicit(self.local_conversation_id);
                    if self.conversation_mcp_enabled {
                        let bootstrap = self.mcp_agent_defaults.bootstrap.clone();
                        if !self.conversation_developer_context.contains(&bootstrap) {
                            if !self.conversation_developer_context.is_empty() {
                                self.conversation_developer_context.push_str("\n\n");
                            }
                            self.conversation_developer_context.push_str(&bootstrap);
                        }
                    }
                    self.persist_current_inference_settings();
                }
            });
            ui.label(if self.conversation_mcp_enabled {
                "MCP ON · this conversation has the agent tool-call protocol."
            } else {
                "MCP OFF · enable above to allow model-proposed tool calls."
            });
            ui.collapsing("Host shell · Fish / Bash / Zsh / Sh", |ui| {
                ui.label(egui::RichText::new(
                    "UNRESTRICTED HOST ACCESS: commands run with your Linux user identity and Fish configuration. The blacklist is a convenience filter, NOT a security sandbox. A malicious model instruction or search result could cause damaging commands when the MCP policy is automatic."
                ).color(egui::Color32::from_rgb(230, 170, 104)));
                ui.checkbox(&mut self.host_shell_settings.enabled, "Enable host shell tool");
                egui::ComboBox::from_label("Shell")
                    .selected_text(&self.host_shell_settings.shell)
                    .show_ui(ui, |ui| {
                        for shell in ["fish", "bash", "zsh", "sh"] {
                            ui.selectable_value(&mut self.host_shell_settings.shell, shell.to_owned(), shell);
                        }
                    });
                ui.label("Blocked command names (one per line, optional):");
                ui.add(egui::TextEdit::multiline(&mut self.host_shell_settings.blocked_commands)
                    .desired_rows(4).desired_width(f32::INFINITY));
                if ui.button("Save host shell configuration").clicked() {
                    self.save_host_shell_settings();
                }
                ui.label("Agent tool: chatarium.builtin::shell.command with {\"command\":\"fish syntax\",\"cwd\":\"/absolute/path\"}. Default: Fish, no blocked commands. Change automatic/ask/disabled in Agent defaults.");
            });
            ui.collapsing("Agent defaults · bootstrap / tool policies", |ui| {
                ui.label("Saved MCP bootstrap and exact per-tool execution permissions.");
                ui.checkbox(
                    &mut self.mcp_agent_defaults.seed_new_conversations,
                    "Enable MCP automatically in new local conversations",
                ).on_hover_text("New local conversations inherit this saved developer bootstrap and enable MCP. Exact automatic-policy tools run; ask-mode tools require user approval.");
                ui.label("Model-facing MCP instructions (editable):");
                ui.add(
                    egui::TextEdit::multiline(&mut self.mcp_agent_defaults.bootstrap)
                        .desired_rows(5)
                        .desired_width(f32::INFINITY),
                );
                ui.label("Requested policies · provider::operation = automatic|ask|manual|disabled");
                ui.collapsing("Local HTTP tools · automatic by default · editable", |ui| {
                    ui.label("Change an individual policy, then press Save MCP defaults. Mathematica code can run with your Linux user privileges.");
                    for (provider, operation, _) in local_http_mcp::registered_tools() {
                        let current = mcp_agent_defaults::tool_policy(
                            &self.mcp_agent_defaults.policy_rules,
                            provider,
                            operation,
                        ).unwrap_or(mcp_agent_defaults::ToolPolicy::Ask);
                        let mut selected = current;
                        ui.horizontal(|ui| {
                            ui.label(operation);
                            egui::ComboBox::from_id_salt(("local-http-policy", provider, operation))
                                .selected_text(selected.name())
                                .show_ui(ui, |ui| {
                                    for policy in [
                                        mcp_agent_defaults::ToolPolicy::Automatic,
                                        mcp_agent_defaults::ToolPolicy::Ask,
                                        mcp_agent_defaults::ToolPolicy::Manual,
                                        mcp_agent_defaults::ToolPolicy::Disabled,
                                    ] {
                                        ui.selectable_value(&mut selected, policy, policy.name());
                                    }
                                });
                        });
                        if selected != current {
                            self.mcp_agent_defaults.policy_rules =
                                mcp_agent_defaults::set_tool_policy_rule(
                                    &self.mcp_agent_defaults.policy_rules,
                                    provider,
                                    operation,
                                    selected,
                                );
                        }
                    }
                });
                ui.add(
                    egui::TextEdit::multiline(&mut self.mcp_agent_defaults.policy_rules)
                        .desired_rows(4)
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal(|ui| {
                    ui.label("Future per-turn tool-call limit");
                    ui.add(egui::DragValue::new(&mut self.mcp_agent_defaults.max_calls)
                        .range(1..=32));
                });
                ui.horizontal(|ui| {
                    if ui.button("Save MCP defaults").clicked() {
                        self.save_mcp_agent_defaults();
                    }
                    if ui.button("Copy bootstrap").clicked() {
                        ctx.copy_text(self.mcp_agent_defaults.bootstrap.clone());
                    }
                    if ui.button("Enable MCP in this chat").clicked() {
                        self.inference_settings.mark_mcp_activation_explicit(self.local_conversation_id);
                        let bootstrap = self.mcp_agent_defaults.bootstrap.clone();
                        if !self.conversation_developer_context.contains(&bootstrap) {
                            if !self.conversation_developer_context.is_empty() {
                                self.conversation_developer_context.push_str("\n\n");
                            }
                            self.conversation_developer_context.push_str(&bootstrap);
                            self.persist_current_inference_settings();
                        }
                        self.conversation_mcp_enabled = true;
                        self.persist_current_inference_settings();
                        self.status = "MCP proposal protocol enabled for this chat. Exact standing tool policies may execute automatically; ask-mode tools pause for approval.".to_owned();
                    }
                });
                ui.label("Automatic execution requires a saved exact provider::tool rule. Unmatched tools ask for approval. The agent resumes after a durable result.");
            });

            if ui.add_enabled(
                cfg!(target_os = "linux")
                    && !self.tool_command_pending
                    && self.persist_tx.is_some(),
                egui::Button::new("Check Linux sandbox host · no tool call"),
            ).on_hover_text(
                "Runs only the fixed root-owned /usr/bin/true under the actual network-isolated namespace and resource-limit policy. No configured provider launches; no journal dispatch or permission is consumed.",
            ).clicked() {
                self.check_sandbox_host_readiness();
            }
            if let Some(observation) = &self.last_sandbox_host_check {
                match observation {
                    Ok(()) => {
                        ui.label(
                            egui::RichText::new("Last Linux sandbox check: READY · fixed /usr/bin/true only")
                                .color(egui::Color32::from_rgb(106, 169, 128)),
                        );
                    }
                    Err(detail) => {
                        let category = local_stdio_host_diagnostics::failure_category(detail);
                        ui.label(
                            egui::RichText::new(format!(
                                "Last Linux sandbox check: NOT READY · {category}"
                            ))
                            .color(egui::Color32::from_rgb(186, 108, 108)),
                        );
                        ui.label(local_stdio_host_diagnostics::failure_explanation(category));
                        ui.collapsing("Bounded host failure detail · diagnostic only", |ui| {
                            ui.label(egui::RichText::new(detail).monospace());
                        });
                    }
                }
                ui.label(
                    egui::RichText::new(
                        "Point-in-time observation only. No provider launched, journal written, or route permission consumed. Readiness cannot authorize an MCP call."
                    )
                    .size(9.0)
                    .color(egui::Color32::from_rgb(139, 143, 153)),
                );
            }
            match replay_tool_provider_audit(&self.events) {
                Err(error) => {
                    ui.label(
                        egui::RichText::new(format!(
                            "tool provider projection blocked: {error}"
                        ))
                        .size(9.0)
                        .color(egui::Color32::from_rgb(186, 108, 108)),
                    );
                }
                Ok(providers) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(
                                &mut self.tool_provider_name_draft,
                            )
                            .desired_width(180.0)
                            .hint_text("Provider name"),
                        );
                        if ui
                            .add_enabled(
                                !self.tool_command_pending
                                    && !self
                                        .tool_provider_name_draft
                                        .is_empty()
                                    && self.persist_tx.is_some(),
                                egui::Button::new("Register provider"),
                            )
                            .clicked()
                        {
                            self.register_tool_provider();
                        }
                    });

                    if ui
                        .button("Fill hello / time / shell MCP demo")
                        .on_hover_text(
                            "Prefills local drafts only. Does not install a server, register, activate, approve, or run anything.",
                        )
                        .clicked()
                    {
                        self.tool_provider_name_draft = "chatarium.demo".to_owned();
                        self.tool_stdio_executable_draft =
                            "/usr/bin/chatarium-mcp-demo".to_owned();
                        self.tool_stdio_argv_draft.clear();
                        self.tool_stdio_operations_draft =
                            "hello\ntime.now\nshell.command\nchatarium.internal.tools-list"
                                .to_owned();
                        self.tool_operation_draft = "hello".to_owned();
                        self.tool_arguments_draft = "{}".to_owned();
                        self.status = "MCP demo fields prepared. Register a provider, bind its endpoint, configure and activate it, then manually Record / Allow / Run. The trusted /usr/bin demo binary must be installed separately.".to_owned();
                    }

                    if providers.is_empty() {
                        ui.label(
                            egui::RichText::new(
                                "No external MCP providers registered. Built-in tools are available to MCP-enabled conversations.",
                            )
                            .size(9.0)
                            .color(egui::Color32::from_rgb(139, 143, 153)),
                        );
                    } else {
                        if self.tool_selected_provider.is_none()
                            || !providers.iter().any(|provider| {
                                Some(provider.provider_id)
                                    == self.tool_selected_provider
                            })
                        {
                            self.tool_selected_provider =
                                Some(providers[0].provider_id);
                        }

                        let selected_text = self
                            .tool_selected_provider
                            .and_then(|provider_id| {
                                providers.iter().find(|provider| {
                                    provider.provider_id == provider_id
                                })
                            })
                            .map(|provider| {
                                format!(
                                    "{} · provider {}",
                                    provider.name.as_str(),
                                    provider.provider_id.get()
                                )
                            })
                            .unwrap_or_else(|| "Select provider".to_owned());

                        egui::ComboBox::from_id_salt("tool-provider-select")
                            .selected_text(selected_text)
                            .show_ui(ui, |ui| {
                                for provider in &providers {
                                    ui.selectable_value(
                                        &mut self.tool_selected_provider,
                                        Some(provider.provider_id),
                                        format!(
                                            "{} · provider {}",
                                            provider.name.as_str(),
                                            provider.provider_id.get()
                                        ),
                                    );
                                }
                            });

                        // Setup advice is observational: failed projections cannot
                        // silently claim any permission or readiness.
                        if let Some(selected) = self.tool_selected_provider.and_then(|id| {
                            providers.iter().find(|provider| provider.provider_id == id)
                        }) {
                            let builtin = selected.name.as_str()
                                == local_tool_adapter::BUILTIN_PROVIDER_NAME;
                            let setup = if builtin {
                                Some(mcp_provider_workflow::ProviderWorkflowFacts {
                                    builtin: true,
                                    endpoint_bound: selected.endpoint_binding.is_some(),
                                    transport_configured: false,
                                    activated: false,
                                    source_addressable: false,
                                    catalog_inspection_allowlisted: false,
                                })
                            } else {
                                match (
                                    replay_tool_transport_config_audit(&self.events),
                                    replay_tool_provider_activation_audit(&self.events),
                                    replay_local_routing_directory(&self.events),
                                ) {
                                    (Ok(configs), Ok(activations), Ok(directory)) => {
                                        let config = configs.iter().find(|record| {
                                            record.provider_id == selected.provider_id
                                        });
                                        Some(mcp_provider_workflow::ProviderWorkflowFacts {
                                            builtin: false,
                                            endpoint_bound: selected.endpoint_binding.is_some(),
                                            transport_configured: config.is_some(),
                                            activated: activations.iter().find(|record| {
                                                record.provider_id == selected.provider_id
                                            }).is_some_and(|record| record.active),
                                            source_addressable: directory.iter().any(|entry| {
                                                entry.conversation_id == self.local_conversation_id
                                                    && entry.current_session_phase.accepts_ordinary_turns()
                                            }),
                                            catalog_inspection_allowlisted: config.is_some_and(|record| {
                                                record.config.allowed_operations().iter().any(|operation| {
                                                    operation.as_str() == MCP_LIST_TOOLS_OPERATION
                                                })
                                            }),
                                        })
                                    }
                                    _ => None,
                                }
                            };
                            ui.group(|ui| {
                                ui.label(egui::RichText::new("Selected provider · setup guidance (read-only)").strong());
                                if let Some(facts) = setup {
                                    let step = mcp_provider_workflow::next_step(facts);
                                    ui.label(egui::RichText::new(step.heading()).strong());
                                    ui.label(step.instruction());
                                    let note = mcp_provider_workflow::catalog_note(facts);
                                    if !note.is_empty() {
                                        ui.label(egui::RichText::new(note).small());
                                    }
                                } else {
                                    ui.label("Setup guidance unavailable: a provider, activation, or routing audit could not be replayed. Review the detailed audit errors below.");
                                }
                                ui.label(egui::RichText::new(
                                    "Guidance does not bind endpoints, activate providers, record calls, approve routes, or execute MCP tools."
                                ).small());
                            });
                        }

                        ui.collapsing("Provider registry", |ui| {
                            for provider in &providers {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} · provider {}",
                                            provider.name.as_str(),
                                            provider.provider_id.get(),
                                        ))
                                        .monospace()
                                        .size(9.0),
                                    );
                                    match provider.endpoint_binding {
                                        Some(binding) => {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "endpoint {} · registered",
                                                    binding.endpoint_id().get()
                                                ))
                                                .monospace()
                                                .size(9.0),
                                            );
                                        }
                                        None => {
                                            ui.label(
                                                egui::RichText::new(
                                                    "NOT ADDRESSABLE",
                                                )
                                                .monospace()
                                                .size(9.0),
                                            );
                                            if ui
                                                .add_enabled(
                                                    !self.tool_command_pending
                                                        && self.persist_tx.is_some(),
                                                    egui::Button::new(
                                                        "Bind routing endpoint",
                                                    ),
                                                )
                                                .clicked()
                                            {
                                                self.bind_tool_provider_endpoint(
                                                    provider.provider_id,
                                                );
                                            }
                                        }
                                    }
                                });
                            }
                        });

                        ui.collapsing("External MCP stdio · configure / activate", |ui| {
                            match (
                                replay_tool_transport_config_audit(&self.events),
                                replay_tool_provider_activation_audit(&self.events),
                            ) {
                                (Ok(configs), Ok(activations)) => {
                                    if let Some(provider_id) = self.tool_selected_provider {
                                        if let Some(provider) = providers.iter().find(|p| p.provider_id == provider_id) {
                                            if provider.name.as_str() == local_tool_adapter::BUILTIN_PROVIDER_NAME {
                                                ui.label("Local builtins hello and tools.list cannot use external transport.");
                                            } else if provider.endpoint_binding.is_none() {
                                                ui.label("Bind this provider's routing endpoint first.");
                                            } else if let Some(configured) = configs.iter().find(|r| r.provider_id == provider_id) {
                                                ui.label(egui::RichText::new(format!(
                                                    "Executable: {}", configured.config.executable()
                                                )).monospace());
                                                ui.label(egui::RichText::new(format!(
                                                    "argv: {:?}", configured.config.args()
                                                )).monospace());
                                                ui.label(egui::RichText::new(format!(
                                                    "Allowlist: {}", configured.config.allowed_operations().iter()
                                                        .map(|op| op.as_str()).collect::<Vec<_>>().join(", ")
                                                )).monospace());
                                                let active = activations.iter()
                                                    .find(|r| r.provider_id == provider_id)
                                                    .is_some_and(|r| r.active);
                                                ui.label(if active {
                                                    "ACTIVATED (individual calls still require Allow and one-shot Run)"
                                                } else {
                                                    "INACTIVE (execution disabled)"
                                                });
                                                if ui.add_enabled(
                                                    !self.tool_command_pending && self.persist_tx.is_some(),
                                                    egui::Button::new(if active {
                                                        "Deactivate provider"
                                                    } else {
                                                        "Activate · inspect Linux executable"
                                                    }),
                                                ).on_hover_text(
                                                    "Explicit user decision with Linux metadata inspection on the persistence worker. No process is spawned or tool call dispatched.",
                                                ).clicked() {
                                                    self.decide_stdio_provider_activation(
                                                        provider_id,
                                                        configured.configured_sequence,
                                                        if active {
                                                            ProviderActivationDecision::Deactivate
                                                        } else {
                                                            ProviderActivationDecision::Activate
                                                        },
                                                    );
                                                }
                                            } else {
                                                ui.label("Configure immutable Linux stdio transport. No execution occurs.");
                                                ui.label("Executable · absolute Linux path:");
                                                ui.add(egui::TextEdit::singleline(
                                                    &mut self.tool_stdio_executable_draft
                                                ).desired_width(330.0).hint_text("/usr/bin/example-mcp"));
                                                ui.label("argv · one exact argument per line (no shell):");
                                                ui.add(egui::TextEdit::multiline(
                                                    &mut self.tool_stdio_argv_draft
                                                ).desired_width(330.0).desired_rows(2));
                                                ui.label("Allowed operations · one name per line:");
                                                ui.add(egui::TextEdit::multiline(
                                                    &mut self.tool_stdio_operations_draft
                                                ).desired_width(330.0).desired_rows(2).hint_text("search"));
                                                if ui.add_enabled(
                                                    !self.tool_command_pending
                                                        && self.persist_tx.is_some()
                                                        && !self.tool_stdio_executable_draft.is_empty()
                                                        && !self.tool_stdio_operations_draft.is_empty(),
                                                    egui::Button::new("Save inert stdio configuration"),
                                                ).clicked() {
                                                    self.configure_stdio_tool_provider(provider_id);
                                                }
                                                ui.label("One-shot immutable configuration; activation is a separate approval.");
                                            }
                                        }
                                    }
                                }
                                (Err(error), _) | (_, Err(error)) => {
                                    ui.label(format!("External provider audit blocked: {error}"));
                                }
                            }
                        });

                        let provider_addressable = self
                            .tool_selected_provider
                            .and_then(|provider_id| {
                                providers.iter().find(|provider| {
                                    provider.provider_id == provider_id
                                })
                            })
                            .is_some_and(|provider| {
                                provider.endpoint_binding.is_some()
                            });
                        let source_addressable =
                            replay_local_routing_directory(&self.events)
                                .map(|directory| {
                                    directory.iter().any(|entry| {
                                        entry.conversation_id
                                            == self.local_conversation_id
                                            && entry
                                                .current_session_phase
                                                .accepts_ordinary_turns()
                                    })
                                })
                                .unwrap_or(false);

                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Immutable call intent").strong(),
                        );
                        ui.add(
                            egui::TextEdit::singleline(
                                &mut self.tool_operation_draft,
                            )
                            .desired_width(220.0)
                            .hint_text("Operation name"),
                        );
                        ui.add(
                            egui::TextEdit::multiline(
                                &mut self.tool_arguments_draft,
                            )
                            .desired_width(360.0)
                            .desired_rows(3)
                            .hint_text(
                                "Exact JSON arguments or legacy <tool_call> envelope",
                            ),
                        );
                        let list_allowed = self
                            .tool_selected_provider
                            .and_then(|id| {
                                replay_tool_transport_config_audit(&self.events)
                                    .ok()
                                    .and_then(|configs| {
                                        configs.into_iter().find(|cfg| {
                                            cfg.provider_id == id
                                        })
                                    })
                            })
                            .is_some_and(|cfg| {
                                cfg.config.allowed_operations().iter().any(|op| {
                                    op.as_str() == MCP_LIST_TOOLS_OPERATION
                                })
                            });
                        if ui
                            .add_enabled(
                                list_allowed && !self.tool_command_pending,
                                egui::Button::new("Prepare approved tools/list inspection"),
                            )
                            .on_hover_text(
                                "Only prepares an exact MCP 2026 tools/list request. This provider must explicitly allowlist chatarium.internal.tools-list. Record, Allow and Run are still separate user actions. No provider is executed by this button.",
                            )
                            .clicked()
                        {
                            self.tool_operation_draft =
                                MCP_LIST_TOOLS_OPERATION.to_owned();
                            self.tool_arguments_draft = "{}".to_owned();
                            self.status =
                                "tools/list draft prepared; record, Allow and Run separately".to_owned();
                        }
                        if self.tool_arguments_draft.trim_start().starts_with("<tool_call")
                            && ui
                                .add_enabled(
                                    !self.tool_command_pending,
                                    egui::Button::new("Read legacy envelope"),
                                )
                                .on_hover_text(
                                    "Validate the recovered XML-like tool call and fill its operation name. Keeps exact original text. No durable call or execution occurs.",
                                )
                                .clicked()
                        {
                            match parse_legacy_tool_call(&self.tool_arguments_draft) {
                                Ok(parsed) => {
                                    self.tool_operation_draft = parsed.name;
                                    self.status = format!(
                                        "legacy tool envelope {} parsed · no call recorded or executed",
                                        parsed.id,
                                    );
                                }
                                Err(error) => {
                                    self.status = format!(
                                        "legacy tool envelope rejected: {error:?}",
                                    );
                                }
                            }
                        }
                        if ui
                            .add_enabled(
                                !self.tool_command_pending
                                    && provider_addressable
                                    && source_addressable
                                    && !self.tool_operation_draft.is_empty()
                                    && self.persist_tx.is_some(),
                                egui::Button::new(
                                    "Record call + propose approval route",
                                ),
                            )
                            .on_hover_text(
                                "Records immutable local intent and a RequireApproval ToolCall route. Does not execute the tool.",
                            )
                            .clicked()
                        {
                            self.propose_tool_call();
                        }
                        if !source_addressable {
                            ui.label(
                                egui::RichText::new(
                                    "Current conversation needs an active routing endpoint before it can originate a tool call.",
                                )
                                .size(9.0)
                                .color(egui::Color32::from_rgb(139, 143, 153)),
                            );
                        }
                    }

                    ui.add_space(5.0);
                    ui.label(
                        egui::RichText::new("Tool call audit").strong().size(10.0),
                    );
                    match replay_tool_call_audit(&self.events) {
                        Err(error) => {
                            ui.label(
                                egui::RichText::new(format!(
                                    "tool call projection blocked: {error}"
                                ))
                                .size(9.0)
                                .color(egui::Color32::from_rgb(186, 108, 108)),
                            );
                        }
                        Ok(calls) if calls.is_empty() => {
                            ui.label(
                                egui::RichText::new(
                                    "no immutable tool calls recorded",
                                )
                                .size(9.0)
                                .color(egui::Color32::from_rgb(139, 143, 153)),
                            );
                        }
                        Ok(calls) => {
                            match replay_unresolved_external_tool_dispatches(&self.events) {
                                Ok(unresolved) if !unresolved.is_empty() => {
                                    ui.collapsing(
                                        format!("UNRESOLVED EXTERNAL DISPATCHES · {}", unresolved.len()),
                                        |ui| {
                                            ui.label(
                                                "A route permit was durably consumed, but no tool outcome is recorded. A process may or may not have run. No automatic retries.",
                                            );
                                            for item in &unresolved {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "call {} · route {} · provider {} · dispatched #{}",
                                                        item.call_id.get(),
                                                        item.route_id.get(),
                                                        item.provider_id.get(),
                                                        item.dispatch_sequence,
                                                    ))
                                                    .monospace()
                                                    .size(9.0),
                                                );
                                            }
                                        },
                                    );
                                }
                                Err(error) => {
                                    ui.label(format!(
                                        "Unresolved tool dispatch audit blocked: {error}"
                                    ));
                                }
                                Ok(_) => {}
                            }
                            let routes =
                                replay_routing_audit(&self.events).ok();
                            let outcomes =
                                match replay_tool_call_outcome_audit(&self.events) {
                                    Ok(outcomes) => Some(outcomes),
                                    Err(error) => {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "tool outcome projection blocked: {error}"
                                            ))
                                            .size(9.0)
                                            .color(egui::Color32::from_rgb(
                                                186, 108, 108,
                                            )),
                                        );
                                        None
                                    }
                                };
                            let tool_context_records =
                                match replay_tool_result_context_audit(&self.events) {
                                    Ok(records) => Some(records),
                                    Err(error) => {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "tool result context projection blocked: {error}"
                                            ))
                                            .size(9.0)
                                            .color(egui::Color32::from_rgb(
                                                186, 108, 108,
                                            )),
                                        );
                                        None
                                    }
                                };
                            // Focus is a read-only projection of durable records.
                            // Execution is still guarded independently by the
                            // original approval, activation and sandbox gates.
                            if let (Some(routes), Some(outcomes)) =
                                (&routes, &outcomes)
                            {
                                // Build bounded per-frame lookups once. The
                                // focus must not compare every call to every
                                // historical outcome while rendering.
                                let indexed_routes: BTreeMap<_, _> = routes
                                    .iter()
                                    .map(|route| (route.request.id, route))
                                    .collect();
                                let observed_calls: BTreeSet<_> = outcomes
                                    .iter()
                                    .map(|outcome| outcome.call_id)
                                    .collect();
                                let next_review = calls.iter().filter(
                                    |call| self.tool_selected_provider == Some(call.provider_id),
                                ).filter_map(|call| {
                                    let route_id = call.route_id?;
                                    let route = indexed_routes.get(&route_id).copied()?;
                                    let observed = observed_calls.contains(&call.call_id);
                                    let stage = mcp_call_review::classify(
                                        call.route_bound_sequence.is_some(),
                                        route.latest_user_decision,
                                        route.dispatch_sequence.is_some(),
                                        observed,
                                    );
                                    stage.actionable().then_some((call, route, stage))
                                }).max_by_key(|(call, _, _)| call.recorded_sequence);
                                ui.group(|ui| {
                                    ui.label(egui::RichText::new(
                                        "NEXT MANUAL MCP CALL REVIEW"
                                    ).strong());
                                    if let Some((call, route, stage)) = next_review {
                                        ui.label(format!(
                                            "Call {} · route {} · {} · {}",
                                            call.call_id.get(),
                                            route.request.id.get(),
                                            call.operation.as_str(),
                                            stage.label(),
                                        ));
                                        ui.label(format!(
                                            "Provider {} · source session {} · recorded #{}",
                                            call.provider_id.get(),
                                            call.source_session_id.get(),
                                            call.recorded_sequence,
                                        ));
                                        ui.collapsing(
                                            format!("Inspect exact recorded intent · {} bytes", call.arguments_text.len()),
                                            |ui| {
                                                ui.label(egui::RichText::new(call.arguments_text.as_str()).monospace());
                                            },
                                        );
                                        match stage {
                                            mcp_call_review::CallReviewStage::AwaitingDecision => {
                                                ui.label("Review the immutable intent. Allow does not run the tool; execution needs a separate deliberate action.");
                                                let enabled = !self.tool_command_pending
                                                    && self.persist_tx.is_some()
                                                    && !route.gate_state.is_dispatched();
                                                ui.horizontal(|ui| {
                                                    if ui.add_enabled(
                                                        enabled, egui::Button::new("Allow this route")
                                                    ).clicked() {
                                                        self.decide_tool_call_route(
                                                            route.request.id, RouteUserDecision::Allow
                                                        );
                                                    }
                                                    if ui.add_enabled(
                                                        enabled, egui::Button::new("Deny this route")
                                                    ).clicked() {
                                                        self.decide_tool_call_route(
                                                            route.request.id, RouteUserDecision::Deny
                                                        );
                                                    }
                                                });
                                            }
                                            mcp_call_review::CallReviewStage::ApprovedForSeparateReview => {
                                                let builtin_hello = providers.iter().find(|provider| {
                                                    provider.provider_id == call.provider_id
                                                }).is_some_and(|provider| {
                                                    local_tool_adapter::supports_builtin(
                                                        provider.name.as_str(),
                                                        call.operation.as_str(),
                                                    )
                                                });
                                                if builtin_hello {
                                                    ui.label(if call.operation.as_str() == host_shell_mcp::OPERATION {
                                                        "Host shell runs as your Linux user, using your real home and PATH. This is NOT sandboxed."
                                                    } else {
                                                        "Approved local builtin. Run separately."
                                                    });
                                                    if ui.add_enabled(
                                                        matches!(
                                                            route.gate_state,
                                                            RouteGateState::Allowed {
                                                                by: DecisionAuthority::User,
                                                            }
                                                        )
                                                            && !self.tool_command_pending
                                                            && self.persist_tx.is_some(),
                                                        egui::Button::new(if call.operation.as_str() == host_shell_mcp::OPERATION {
                                                            "Run approved host shell · once"
                                                        } else {
                                                            "Run approved local builtin · once"
                                                        }),
                                                    ).clicked() {
                                                        if call.operation.as_str() == host_shell_mcp::OPERATION {
                                                            self.dispatch_host_shell(route.request.id);
                                                        } else {
                                                            self.dispatch_builtin_hello(route.request.id);
                                                        }
                                                    }
                                                } else if providers.iter().find(|provider| {
                                                    provider.provider_id == call.provider_id
                                                }).is_some_and(|provider| local_http_mcp::allowed_operation(
                                                    provider.name.as_str(), call.operation.as_str()
                                                )) {
                                                    ui.label("Fixed loopback Streamable HTTP MCP. An approved call uses the shared local server, not the network-isolated stdio sandbox. It may perform side effects (Mathematica executes Wolfram code).");
                                                    if ui.add_enabled(
                                                        matches!(route.gate_state, RouteGateState::Allowed { by: DecisionAuthority::User })
                                                            && !self.tool_command_pending && self.persist_tx.is_some(),
                                                        egui::Button::new("Run approved localhost MCP call · once"),
                                                    ).clicked() {
                                                        self.dispatch_local_http_mcp(route.request.id);
                                                    }
                                                } else {
                                                    ui.label("User Allow is recorded. Inspect the exact wire request before a separate one-shot Run.");
                                                    ui.collapsing("Review approved wire request and choose Run", |ui| {
                                                    match preview_activated_stdio_tool_invocation(
                                                        &self.events, call.call_id
                                                    ) {
                                                        Ok(preview) => {
                                                            ui.label(format!(
                                                                "Activation #{} · route {}",
                                                                preview.activation_sequence,
                                                                preview.invocation.route_id.get(),
                                                            ));
                                                            ui.label(egui::RichText::new(
                                                                preview.invocation.request_frame.as_str()
                                                            ).monospace());
                                                            match local_stdio_runner::plan_confined_stdio_launch(
                                                                preview.invocation.executable.as_str(),
                                                                &preview.invocation.argv,
                                                            ) {
                                                                Ok(_) => {
                                                                    ui.label("Restricted network-isolated sandbox. Run permanently consumes this approved route; no automatic retries.");
                                                                    if ui.add_enabled(
                                                                        cfg!(target_os = "linux")
                                                                            && !self.tool_command_pending
                                                                            && self.persist_tx.is_some(),
                                                                        egui::Button::new("Run approved MCP call · one shot"),
                                                                    ).clicked() {
                                                                        self.execute_approved_tool(call.call_id);
                                                                    }
                                                                }
                                                                Err(error) => {
                                                                    ui.label(format!("Sandbox plan blocked: {error}"));
                                                                }
                                                            }
                                                        }
                                                        Err(error) => {
                                                            ui.label(format!("Run not eligible: {error}"));
                                                        }
                                                    }
                                                    });
                                                }
                                            }
                                            _ => {}
                                        }
                                    } else {
                                        ui.label("No unconsumed calls await review for the selected provider.");
                                    }
                                    ui.label(egui::RichText::new(
                                        "This focus grants no permission. Allow/Deny and Run remain separate explicit actions."
                                    ).small());
                                });
                            } else {
                                ui.label("Call review focus unavailable: route or outcome audit blocked.");
                            }

                            // Terminal evidence is independently reviewed for
                            // context admission, never for execution authority.
                            if let (Some(outcomes), Some(context_records)) =
                                (&outcomes, &tool_context_records)
                            {
                                if let Some(provider_id) = self.tool_selected_provider {
                                    ui.collapsing("RECENT MCP RESULTS · separate context review", |ui| {
                                        match mcp_result_review::recent_result_queue(
                                            outcomes,
                                            context_records,
                                            provider_id,
                                            self.local_conversation_id,
                                            |outcome| tool_outcome_owning_conversation(&self.events, outcome),
                                        ) {
                                            Ok(queue) if !queue.is_empty() => {
                                                if let Some((call_id, decision)) =
                                                    mcp_result_review::render_result_queue(
                                                        ui,
                                                        &queue,
                                                        &calls,
                                                        !self.tool_command_pending && self.persist_tx.is_some(),
                                                    )
                                                {
                                                    self.decide_tool_result_context(call_id, decision);
                                                }
                                            }
                                            Ok(_) => {
                                                ui.label("No results owned by this conversation among the 16 most recent observations for the selected provider. Historical audit remains available.");
                                            }
                                            Err(error) => {
                                                ui.label(format!(
                                                    "Result review blocked by historical ownership or audit evidence: {error}"
                                                ));
                                            }
                                        }
                                    });
                                }
                            } else {
                                ui.label("Result review unavailable: outcome or context-decision audit cannot be replayed.");
                            }

                            egui::ScrollArea::vertical()
                                .id_salt("tool-call-audit")
                                .max_height(240.0)
                                .show(ui, |ui| {
                                    for call in calls.iter().rev() {
                                        let provider_label = providers
                                            .iter()
                                            .find(|provider| {
                                                provider.provider_id
                                                    == call.provider_id
                                            })
                                            .map(|provider| {
                                                provider.name.as_str().to_owned()
                                            })
                                            .unwrap_or_else(|| {
                                                format!(
                                                    "provider {}",
                                                    call.provider_id.get()
                                                )
                                            });
                                        let route = call.route_id.and_then(
                                            |route_id| {
                                                routes.as_ref().and_then(|routes| {
                                                    routes.iter().find(|route| {
                                                        route.request.id == route_id
                                                    })
                                                })
                                            },
                                        );

                                        ui.group(|ui| {
                                            ui.horizontal_wrapped(|ui| {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "CALL {} · {} · {} · source session {}",
                                                        call.call_id.get(),
                                                        provider_label,
                                                        call.operation.as_str(),
                                                        call.source_session_id.get(),
                                                    ))
                                                    .monospace()
                                                    .size(9.0),
                                                );
                                                if let Some(route) = route {
                                                    ui.label(
                                                        egui::RichText::new(
                                                            format!(
                                                                "route {} · {}",
                                                                route.request.id.get(),
                                                                route_gate_state_label(
                                                                    route.gate_state,
                                                                ),
                                                            ),
                                                        )
                                                        .monospace()
                                                        .size(9.0),
                                                    );

                                                    if local_http_mcp::allowed_operation(provider_label.as_str(), call.operation.as_str())
                                                        && route.dispatch_sequence.is_none()
                                                    {
                                                        ui.label("Known localhost MCP service · exact approval still required.");
                                                        if ui.add_enabled(
                                                            matches!(route.gate_state, RouteGateState::Allowed { by: DecisionAuthority::User })
                                                                && !self.tool_command_pending && self.persist_tx.is_some(),
                                                            egui::Button::new("Run local HTTP MCP · one shot"),
                                                        ).clicked() {
                                                            self.dispatch_local_http_mcp(route.request.id);
                                                        }
                                                    }
                                                    if !local_tool_adapter::supports_hello(
                                                        provider_label.as_str(),
                                                        call.operation.as_str(),
                                                    ) && !local_http_mcp::known_provider(provider_label.as_str())
                                                        && route.dispatch_sequence.is_none() {
                                                        ui.collapsing("Activation-aware MCP review and Run", |ui| {
                                                            match preview_activated_stdio_tool_invocation(&self.events, call.call_id) {
                                                                Ok(preview) => {
                                                                    ui.label(format!(
                                                                        "Active provider · activation #{} · approved route {}",
                                                                        preview.activation_sequence,
                                                                        preview.invocation.route_id.get(),
                                                                    ));
                                                                    ui.label(egui::RichText::new(
                                                                        preview.invocation.request_frame.as_str()
                                                                    ).monospace());
                                                                    ui.label("Preview alone grants NO execution authority.");
                                                                    match local_stdio_runner::plan_confined_stdio_launch(
                                                                        preview.invocation.executable.as_str(),
                                                                        &preview.invocation.argv,
                                                                    ) {
                                                                        Ok(_) => {
                                                                            ui.label(
                                                                                "One-shot confinement: private network, no home, read-only /usr, 32 MiB scratch, 10s wall-time, 8s CPU, 512 MiB memory. Requires root-owned /usr/bin executable and installed bubblewrap + prlimit.",
                                                                            );
                                                                            if ui.add_enabled(
                                                                                cfg!(target_os = "linux")
                                                                                    && !self.tool_command_pending
                                                                                    && self.persist_tx.is_some(),
                                                                                egui::Button::new("Run sandboxed MCP tool · one shot"),
                                                                            ).on_hover_text(
                                                                                "Separate deliberate execution after provider activation and explicit ToolCall Allow. Consumes the route permanently before a sandboxed process can start. No automatic retries.",
                                                                            ).clicked() {
                                                                                self.execute_approved_tool(call.call_id);
                                                                            }
                                                                        }
                                                                        Err(reason) => {
                                                                            ui.label(format!("Sandbox execution unavailable: {reason}"));
                                                                        }
                                                                    }
                                                                }
                                                                Err(error) => { ui.label(format!("Not eligible: {error}")); }
                                                            }
                                                        });
                                                    }
                                                    let can_decide =
                                                        !self.tool_command_pending
                                                            && !route
                                                                .gate_state
                                                                .is_dispatched()
                                                            && self
                                                                .persist_tx
                                                                .is_some();
                                                    if ui
                                                        .add_enabled(
                                                            can_decide
                                                                && route
                                                                    .latest_user_decision
                                                                    != Some(
                                                                        RouteUserDecision::Allow,
                                                                    ),
                                                            egui::Button::new(
                                                                "Allow",
                                                            ),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.decide_tool_call_route(
                                                            route.request.id,
                                                            RouteUserDecision::Allow,
                                                        );
                                                    }
                                                    if ui
                                                        .add_enabled(
                                                            can_decide
                                                                && route
                                                                    .latest_user_decision
                                                                    != Some(
                                                                        RouteUserDecision::Deny,
                                                                    ),
                                                            egui::Button::new(
                                                                "Deny",
                                                            ),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.decide_tool_call_route(
                                                            route.request.id,
                                                            RouteUserDecision::Deny,
                                                        );
                                                    }
                                                    if ui
                                                        .add_enabled(
                                                            local_tool_adapter::supports_hello(
                                                                provider_label.as_str(),
                                                                call.operation.as_str(),
                                                            )
                                                                && matches!(
                                                                    route.gate_state,
                                                                    RouteGateState::Allowed {
                                                                        by: DecisionAuthority::User,
                                                                    }
                                                                )
                                                                && !self.tool_command_pending
                                                                && self.persist_tx.is_some(),
                                                            egui::Button::new(
                                                                "Run local hello",
                                                            ),
                                                        )
                                                        .on_hover_text(
                                                            "Side-effect-free builtin hello only. Requires explicit Allow and consumes the route once. No shell, file, network, or arbitrary MCP execution.",
                                                        )
                                                        .clicked()
                                                    {
                                                        self.dispatch_builtin_hello(
                                                            route.request.id,
                                                        );
                                                    }
                                                } else {
                                                    ui.label(
                                                        egui::RichText::new(
                                                            "UNBOUND CALL INTENT",
                                                        )
                                                        .monospace()
                                                        .size(9.0),
                                                    );
                                                }
                                            });
                                            ui.collapsing(
                                                format!(
                                                    "Exact opaque arguments · {} bytes",
                                                    call.arguments_text.len()
                                                ),
                                                |ui| {
                                                    if call.arguments_text.is_empty()
                                                    {
                                                        ui.label(
                                                            egui::RichText::new(
                                                                "<empty>",
                                                            )
                                                            .monospace()
                                                            .size(9.0),
                                                        );
                                                    } else {
                                                        ui.label(
                                                            egui::RichText::new(
                                                                call.arguments_text
                                                                    .as_str(),
                                                            )
                                                            .size(10.0),
                                                        );
                                                    }
                                                },
                                            );
                                            if let Some(outcome) = outcomes
                                                .as_ref()
                                                .and_then(|outcomes| {
                                                    outcomes.iter().find(|outcome| {
                                                        outcome.call_id == call.call_id
                                                    })
                                                })
                                            {
                                                ui.collapsing(
                                                    format!(
                                                        "ADAPTER {} · route {} · dispatched #{} · observed #{} · {} bytes",
                                                        outcome.kind.stable_name(),
                                                        outcome.route_id.get(),
                                                        outcome.dispatch_sequence,
                                                        outcome.observed_sequence,
                                                        outcome.text.len(),
                                                    ),
                                                    |ui| {
                                                        ui.label(
                                                            egui::RichText::new(
                                                                outcome.text.as_str(),
                                                            )
                                                            .size(10.0),
                                                        );
                                                    },
                                                );
                                                if call.operation.as_str() == MCP_LIST_TOOLS_OPERATION
                                                    && outcome.kind == ToolCallOutcomeKind::Result
                                                {
                                                    ui.collapsing("Provider-advertised tools · untrusted inspection", |ui| {
                                                        match decode_stdio_response(&outcome.text, call.call_id.get()) {
                                                            Ok(McpResponse::Complete(result)) => match parse_tools_list_page(&result) {
                                                                Ok(page) => {
                                                                    ui.label(format!("{} advertised tools in this page; no permissions or allowlist changes", page.tools.len()));
                                                                    let inspection_owned_here =
                                                                        tool_outcome_owning_conversation(&self.events, outcome)
                                                                            .ok()
                                                                            .flatten()
                                                                            == Some(self.local_conversation_id);
                                                                    let source_accepts_calls =
                                                                        replay_local_routing_directory(&self.events)
                                                                            .ok()
                                                                            .is_some_and(|directory| {
                                                                                directory.iter().any(|entry| {
                                                                                    entry.conversation_id == self.local_conversation_id
                                                                                        && entry.current_session_phase.accepts_ordinary_turns()
                                                                                })
                                                                            });
                                                                    let currently_active =
                                                                        replay_tool_provider_activation_audit(&self.events)
                                                                            .ok()
                                                                            .is_some_and(|records| {
                                                                                records.iter().any(|record| {
                                                                                    record.provider_id == call.provider_id
                                                                                        && record.active
                                                                                })
                                                                            });
                                                                    let configured_operations =
                                                                        replay_tool_transport_config_audit(&self.events)
                                                                            .ok()
                                                                            .and_then(|records| {
                                                                                records
                                                                                    .into_iter()
                                                                                    .find(|record| record.provider_id == call.provider_id)
                                                                            })
                                                                            .map(|record| {
                                                                                record.config.allowed_operations()
                                                                                    .iter()
                                                                                    .map(|operation| operation.as_str().to_owned())
                                                                                    .collect::<BTreeSet<_>>()
                                                                            })
                                                                            .unwrap_or_default();
                                                                    for advertised in &page.tools {
                                                                        ui.label(egui::RichText::new(format!(
                                                                            "{} · {}",
                                                                            advertised.name,
                                                                            advertised.title.as_deref().unwrap_or("unnamed tool"),
                                                                        )).monospace());
                                                                        if let Some(description) = &advertised.description {
                                                                            ui.label(description.as_str());
                                                                        }
                                                                        mcp_output_audit::render_catalog_output_schema(ui, advertised);
                                                                        ui.collapsing(
                                                                            format!("Review required-field JSON draft · {}", advertised.name),
                                                                            |ui| {
                                                                                match draft_required_arguments(&advertised.input_schema) {
                                                                                    Ok(draft) => {
                                                                                        ui.label("Only required properties are included. Placeholders are NOT validated values; edit and review before recording.");
                                                                                        ui.label(egui::RichText::new(draft.arguments_json.as_str()).monospace());
                                                                                        for note in &draft.review_notes {
                                                                                            ui.label(note.as_str());
                                                                                        }
                                                                                        let allowed_operation =
                                                                                            ToolOperationName::new(advertised.name.clone())
                                                                                                .is_ok()
                                                                                                && configured_operations.contains(advertised.name.as_str());
                                                                                        let can_prepare = inspection_owned_here
                                                                                            && source_accepts_calls
                                                                                            && currently_active
                                                                                            && allowed_operation
                                                                                            && !self.tool_command_pending;
                                                                                        if ui.add_enabled(
                                                                                            can_prepare,
                                                                                            egui::Button::new("Fill editable tool-call draft · no execution"),
                                                                                        ).on_hover_text(
                                                                                            "Copies only bounded required-field placeholders into the local draft. Requires same owning conversation, current provider activation, and an existing immutable operation allowlist entry. Record, Allow and Run remain separate.",
                                                                                        ).clicked() {
                                                                                            self.tool_selected_provider = Some(call.provider_id);
                                                                                            self.tool_operation_draft = advertised.name.clone();
                                                                                            self.tool_arguments_draft = draft.arguments_json;
                                                                                            self.status = format!(
                                                                                                "drafted {} from untrusted catalog; review {} placeholders, then Record / Allow / Run separately",
                                                                                                advertised.name,
                                                                                                draft.review_notes.len(),
                                                                                            );
                                                                                        }
                                                                                        if !allowed_operation {
                                                                                            ui.label("Unavailable: this tool is not in the provider's immutable allowed operations.");
                                                                                        } else if !inspection_owned_here || !source_accepts_calls {
                                                                                            ui.label("Unavailable: catalog and active source conversation must match.");
                                                                                        } else if !currently_active {
                                                                                            ui.label("Unavailable: provider is not currently active.");
                                                                                        }
                                                                                    }
                                                                                    Err(reason) => {
                                                                                        ui.label(format!("Cannot construct bounded JSON draft: {reason}"));
                                                                                        ui.label("You may still manually author an exact JSON object for a separately allowlisted operation.");
                                                                                    }
                                                                                }
                                                                            },
                                                                        );
                                                                    }
                                                                    if let Some(cursor) = &page.next_cursor {
                                                                        ui.label("Another page is advertised; continuation is NEVER automatic.");
                                                                        if ui.add_enabled(
                                                                            !self.tool_command_pending,
                                                                            egui::Button::new("Prepare next catalog page"),
                                                                        ).on_hover_text(
                                                                            "Only fills a new immutable call draft for the same provider. It does not dispatch or grant permission.",
                                                                        ).clicked() {
                                                                            self.tool_selected_provider = Some(call.provider_id);
                                                                            self.tool_operation_draft = MCP_LIST_TOOLS_OPERATION.to_owned();
                                                                            self.tool_arguments_draft = serde_json::json!({"cursor":cursor}).to_string();
                                                                            self.status = "next catalog page drafted; requires a new approval and Run".to_owned();
                                                                        }
                                                                    }
                                                                }
                                                                Err(error) => {
                                                                    ui.label(format!("Invalid catalog result: {error:?}"));
                                                                }
                                                            },
                                                            _ => {
                                                                ui.label("Catalog result cannot be decoded.");
                                                            }
                                                        }
                                                    });
                                                }
                                                mcp_output_audit::render_snapshot_comparisons(
                                                    ui,
                                                    &self.events,
                                                    &calls,
                                                    outcomes.as_deref().unwrap_or(&[]),
                                                    call,
                                                    outcome,
                                                    self.local_conversation_id,
                                                );
                                                if let Some(records) = &tool_context_records {
                                                    match tool_outcome_owning_conversation(&self.events, outcome) {
                                                        Ok(Some(owner))
                                                            if owner == self.local_conversation_id =>
                                                        {
                                                            let latest = records
                                                                .iter()
                                                                .find(|record| record.call_id == call.call_id)
                                                                .map(|record| record.decision);
                                                            ui.horizontal_wrapped(|ui| {
                                                                ui.label(
                                                                    egui::RichText::new(
                                                                        match latest {
                                                                            Some(ToolResultContextDecision::Admit) => "CONTEXT: ADMITTED",
                                                                            Some(ToolResultContextDecision::Exclude) => "CONTEXT: EXCLUDED",
                                                                            None => "CONTEXT: EXCLUDED · DEFAULT",
                                                                        }
                                                                    )
                                                                    .monospace()
                                                                    .size(9.0),
                                                                );
                                                                let can_change =
                                                                    !self.tool_command_pending
                                                                    && self.persist_tx.is_some();
                                                                if ui.add_enabled(
                                                                    can_change
                                                                        && outcome.text.len() <= MAX_CONTEXT_TOOL_RESULT_BYTES
                                                                        && latest != Some(ToolResultContextDecision::Admit),
                                                                    egui::Button::new("Admit result"),
                                                                ).clicked() {
                                                                    self.decide_tool_result_context(
                                                                        call.call_id,
                                                                        ToolResultContextDecision::Admit,
                                                                    );
                                                                }
                                                                if ui.add_enabled(
                                                                    can_change
                                                                        && latest == Some(ToolResultContextDecision::Admit),
                                                                    egui::Button::new("Exclude result"),
                                                                ).clicked() {
                                                                    self.decide_tool_result_context(
                                                                        call.call_id,
                                                                        ToolResultContextDecision::Exclude,
                                                                    );
                                                                }
                                                            });
                                                            if outcome.text.len() > MAX_CONTEXT_TOOL_RESULT_BYTES {
                                                                ui.label("Result is too large for context admission; exact audit remains available.");
                                                            }
                                                        }
                                                        Ok(Some(_)) => {
                                                            ui.label("Tool result belongs to another local conversation.");
                                                        }
                                                        Ok(None) => {
                                                            ui.label("Tool result has no local conversation owner; context admission disabled.");
                                                        }
                                                        Err(error) => {
                                                            ui.label(format!("tool result ownership blocked: {error}"));
                                                        }
                                                    }
                                                }
                                            } else if route.is_some_and(|route| {
                                                route.gate_state.is_dispatched()
                                            }) {
                                                ui.label(
                                                    egui::RichText::new(
                                                        "DISPATCHED · NO OBSERVED ADAPTER OUTCOME",
                                                    )
                                                    .monospace()
                                                    .size(9.0),
                                                );
                                            }
                                            ui.label(
                                                egui::RichText::new(
                                                    if local_tool_adapter::supports_hello(provider_label.as_str(), call.operation.as_str()) {
                                                        "LOCAL HELLO ONLY · explicit Allow and separate one-shot Run required."
                                                    } else {
                                                        "EXECUTION DISABLED · no general MCP or external tool adapter is attached."
                                                    },
                                                )
                                                .monospace()
                                                .size(9.0)
                                                .color(
                                                    egui::Color32::from_rgb(
                                                        139, 143, 153,
                                                    ),
                                                ),
                                            );
                                        });
                                    }
                                });
                        }
                    }
                }
            }

            if self.tool_command_pending {
                ui.spinner();
            }
        });
    }
}
