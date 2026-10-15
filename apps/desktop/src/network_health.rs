//! Lightweight, unauthenticated HTTPS reachability evidence for the ChatGPT route.
//! This is not a promise that an authenticated model request will be accepted.
use std::collections::VecDeque;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;

const PROBE_INTERVAL: Duration = Duration::from_secs(20);
const MAX_PROBE_AGE: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_HISTORY: usize = 5;
const SLOW_THRESHOLD_MS: u64 = 1_800;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Http(u16),
    Failed(String),
    Unavailable(String),
}

#[derive(Debug, Clone)]
struct Probe {
    outcome: Outcome,
    latency_ms: Option<u64>,
    measured_at: Instant,
}

impl Probe {
    fn is_reachable(&self) -> bool {
        matches!(self.outcome, Outcome::Http(100..=499))
    }
}

fn parse_curl_result(output: &std::process::Output, elapsed: Duration) -> Outcome {
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Outcome::Failed(detail.trim().chars().take(140).collect());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let status = text.split_whitespace().next().and_then(|value| value.parse::<u16>().ok());
    match status {
        Some(code @ 100..=599) => Outcome::Http(code),
        _ => Outcome::Failed(format!("No valid HTTP status after {} ms", elapsed.as_millis())),
    }
}

fn check_https() -> Probe {
    let start = Instant::now();
    // No credentials, cookies, inference calls, or large downloads. curl honors
    // host TLS verification and ordinary proxy settings. HTTP 401/403 still
    // prove HTTPS reachability, but never prove authentication.
    let response = Command::new("curl")
        .args([
            "--head",
            "--silent",
            "--show-error",
            "--location",
            "--connect-timeout", "3",
            "--max-time", "6",
            "--output", "/dev/null",
            "--write-out", "%{http_code}",
            "https://chatgpt.com/",
        ])
        .output();
    let elapsed = start.elapsed();
    let outcome = match response {
        Ok(output) => parse_curl_result(&output, elapsed),
        Err(error) => Outcome::Unavailable(format!("curl unavailable: {error}")),
    };
    Probe {
        outcome,
        latency_ms: Some(elapsed.as_millis().min(u128::from(u64::MAX)) as u64),
        measured_at: Instant::now(),
    }
}

pub struct NetworkHealth {
    tx: Sender<Probe>,
    rx: Receiver<Probe>,
    last: Option<Probe>,
    history: VecDeque<bool>,
    pending_since: Option<Instant>,
    next_probe: Instant,
}

pub struct Display {
    pub label: String,
    pub color: egui::Color32,
    pub tooltip: String,
}

impl Default for NetworkHealth {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            last: None,
            history: VecDeque::new(),
            pending_since: None,
            next_probe: Instant::now(),
        }
    }
}

impl NetworkHealth {
    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(report) = self.rx.try_recv() {
            self.pending_since = None;
            self.next_probe = Instant::now() + PROBE_INTERVAL;
            if self.history.len() == MAX_HISTORY {
                self.history.pop_front();
            }
            self.history.push_back(report.is_reachable());
            self.last = Some(report);
        }

        // A lost probe thread must never leave the UI permanently "checking".
        if self.pending_since.is_some_and(|start| start.elapsed() > PROBE_TIMEOUT + Duration::from_secs(3)) {
            self.pending_since = None;
            self.next_probe = Instant::now() + PROBE_INTERVAL;
            self.last = Some(Probe {
                outcome: Outcome::Failed("HTTPS health check timed out".to_owned()),
                latency_ms: None,
                measured_at: Instant::now(),
            });
            if self.history.len() == MAX_HISTORY {
                self.history.pop_front();
            }
            self.history.push_back(false);
        }

        if self.pending_since.is_none() && Instant::now() >= self.next_probe {
            let tx = self.tx.clone();
            let ctx = ctx.clone();
            match thread::Builder::new().name("chatarium-https-probe".to_owned()).spawn(move || {
                let report = check_https();
                let _ = tx.send(report);
                ctx.request_repaint();
            }) {
                Ok(_) => self.pending_since = Some(Instant::now()),
                Err(error) => {
                    self.next_probe = Instant::now() + PROBE_INTERVAL;
                    self.last = Some(Probe {
                        outcome: Outcome::Unavailable(format!("Probe thread unavailable: {error}")),
                        latency_ms: None,
                        measured_at: Instant::now(),
                    });
                }
            }
        }
    }

    pub fn is_offline(&self) -> bool {
        self.last.as_ref().is_some_and(|probe| {
            probe.measured_at.elapsed() <= MAX_PROBE_AGE
                && matches!(probe.outcome, Outcome::Failed(_) | Outcome::Unavailable(_))
        })
    }

    pub fn display(&self) -> Display {
        let (passed, total) = (self.history.iter().filter(|ok| **ok).count(), self.history.len());
        let Some(probe) = &self.last else {
            return Display {
                label: "NETWORK: checking HTTPS…".to_owned(),
                color: egui::Color32::from_rgb(225, 194, 108),
                tooltip: "Checking https://chatgpt.com/ over TLS. No message is sent.".to_owned(),
            };
        };
        let age = probe.measured_at.elapsed();
        if age > MAX_PROBE_AGE {
            return Display {
                label: format!("NETWORK: STALE · last check {}s ago", age.as_secs()),
                color: egui::Color32::from_rgb(225, 194, 108),
                tooltip: "The last connectivity measurement is too old to trust.".to_owned(),
            };
        }
        let latency = probe.latency_ms.map_or_else(|| "n/a".to_owned(), |ms| format!("{ms}ms"));
        let (state, color) = match &probe.outcome {
            Outcome::Http(500..=599) => ("SERVER ERROR", egui::Color32::from_rgb(236, 139, 106)),
            Outcome::Http(400..=499) => ("HTTPS REACHED", egui::Color32::from_rgb(225, 194, 108)),
            Outcome::Http(_) if probe.latency_ms.is_some_and(|ms| ms >= SLOW_THRESHOLD_MS) => {
                ("SLOW", egui::Color32::from_rgb(225, 194, 108))
            }
            Outcome::Http(_) if total >= 3 && passed + 1 < total => {
                ("UNSTABLE", egui::Color32::from_rgb(225, 194, 108))
            }
            Outcome::Http(_) => ("HTTPS OK", egui::Color32::from_rgb(126, 210, 156)),
            Outcome::Failed(_) => ("UNREACHABLE", egui::Color32::from_rgb(238, 119, 119)),
            Outcome::Unavailable(_) => ("CHECK UNAVAILABLE", egui::Color32::from_rgb(225, 194, 108)),
        };
        let extra = match &probe.outcome {
            Outcome::Http(code) => format!("HTTP {code}"),
            Outcome::Failed(error) | Outcome::Unavailable(error) => error.clone(),
        };
        Display {
            label: format!("NETWORK: {state} · {latency} · {passed}/{total} checks · {}s ago", age.as_secs()),
            color,
            tooltip: format!(
                "HTTPS HEAD https://chatgpt.com/ — {extra}. Measured every ~20s, with 6s timeout. This checks website reachability, not browser authentication, actual model availability, bandwidth, or delivery of a particular request. Request outcomes are shown separately."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_and_http_errors_are_distinguished() {
        let output = std::process::Output {
            status: std::process::Command::new("true").status().unwrap(),
            stdout: b"403".to_vec(),
            stderr: Vec::new(),
        };
        assert_eq!(parse_curl_result(&output, Duration::from_millis(100)), Outcome::Http(403));
        let ok = Probe { outcome: Outcome::Http(403), latency_ms: Some(100), measured_at: Instant::now() };
        assert!(ok.is_reachable());
        let server_error = Probe { outcome: Outcome::Http(503), ..ok };
        assert!(!server_error.is_reachable());
    }

    #[test]
    fn unknown_before_first_probe_and_stale_after_age_limit() {
        let mut state = NetworkHealth::default();
        assert!(state.display().label.contains("checking"));
        state.last = Some(Probe {
            outcome: Outcome::Http(200),
            latency_ms: Some(40),
            measured_at: Instant::now() - MAX_PROBE_AGE - Duration::from_secs(1),
        });
        assert!(state.display().label.contains("STALE"));
    }

    #[test]
    fn failed_probe_is_not_reported_as_connected() {
        let mut state = NetworkHealth::default();
        state.last = Some(Probe {
            outcome: Outcome::Failed("timeout".to_owned()),
            latency_ms: None,
            measured_at: Instant::now(),
        });
        assert!(state.is_offline());
        assert!(state.display().label.contains("UNREACHABLE"));
    }
}
