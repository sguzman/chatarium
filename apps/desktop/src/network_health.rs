//! Independent, lightweight HTTPS health evidence for the actual Responses API
//! and the ChatGPT website. Neither check sends a model request or credentials.
use std::collections::VecDeque;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;

const API_URL: &str = "https://api.openai.com/v1/responses";
const SITE_URL: &str = "https://chatgpt.com/";
const PROBE_INTERVAL: Duration = Duration::from_secs(20);
const MAX_PROBE_AGE: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_HISTORY: usize = 5;
const SLOW_THRESHOLD_MS: u64 = 1_800;
const SEVERE_THRESHOLD_MS: u64 = 4_000;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Http(u16),
    Failed(String),
    Unavailable(String),
}

impl Outcome {
    fn reachable(&self) -> bool {
        // 401 and 403 are expected from unauthenticated API checks. They prove
        // HTTP reachability, NOT valid ChatGPT credentials or POST success.
        matches!(self, Self::Http(100..=499))
    }

    fn detail(&self) -> String {
        match self {
            Self::Http(code) => format!("HTTP {code}"),
            Self::Failed(detail) | Self::Unavailable(detail) => detail.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct Check {
    outcome: Outcome,
    elapsed_ms: Option<u64>,
}

impl Check {
    fn timed_out() -> Self {
        Self {
            outcome: Outcome::Failed("HTTPS health check timed out".to_owned()),
            elapsed_ms: None,
        }
    }

    fn label(&self) -> &'static str {
        match &self.outcome {
            Outcome::Http(500..=599) => "SERVER ERROR",
            Outcome::Http(_) => "REACHED",
            Outcome::Failed(_) => "DOWN",
            Outcome::Unavailable(_) => "UNKNOWN",
        }
    }

    fn time_label(&self) -> String {
        self.elapsed_ms
            .map_or_else(|| "--".to_owned(), |ms| format!("{ms}ms"))
    }

    fn severe(&self) -> bool {
        self.elapsed_ms.is_some_and(|ms| ms >= SEVERE_THRESHOLD_MS)
    }

    fn slow(&self) -> bool {
        self.elapsed_ms.is_some_and(|ms| ms >= SLOW_THRESHOLD_MS)
    }
}

#[derive(Debug, Clone)]
struct Probe {
    api: Check,
    site: Check,
    measured_at: Instant,
}

fn parse_curl_result(output: &std::process::Output, elapsed: Duration) -> Outcome {
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Outcome::Failed(detail.trim().chars().take(140).collect());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let status = text
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<u16>().ok());
    match status {
        Some(code @ 100..=599) => Outcome::Http(code),
        _ => Outcome::Failed(format!(
            "No valid HTTP status after {} ms",
            elapsed.as_millis()
        )),
    }
}

fn check_https(url: &'static str) -> Check {
    let start = Instant::now();
    // No authentication, cookies, model payload, redirects or downloads.
    // HTTPS timing includes DNS/TCP/TLS/HTTP and is not a network ICMP ping.
    let response = Command::new("curl")
        .args([
            "--head",
            "--silent",
            "--show-error",
            "--connect-timeout",
            "3",
            "--max-time",
            "6",
            "--output",
            "/dev/null",
            "--write-out",
            "%{http_code}",
            url,
        ])
        .output();
    let elapsed = start.elapsed();
    let outcome = match response {
        Ok(output) => parse_curl_result(&output, elapsed),
        Err(error) => Outcome::Unavailable(format!("curl unavailable: {error}")),
    };
    Check {
        outcome,
        elapsed_ms: Some(elapsed.as_millis().min(u128::from(u64::MAX)) as u64),
    }
}

fn check_routes() -> Probe {
    // Parallel bounded checks make a broken website unable to delay the API
    // reading (and vice versa) by an additional six seconds.
    let (api, site) = thread::scope(|scope| {
        let api = scope.spawn(|| check_https(API_URL));
        let site = scope.spawn(|| check_https(SITE_URL));
        let api = api.join().unwrap_or_else(|_| Check::timed_out());
        let site = site.join().unwrap_or_else(|_| Check::timed_out());
        (api, site)
    });
    Probe {
        api,
        site,
        measured_at: Instant::now(),
    }
}

pub struct NetworkHealth {
    tx: Sender<Probe>,
    rx: Receiver<Probe>,
    last: Option<Probe>,
    history: VecDeque<(bool, bool)>,
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
    /// On-demand checks never overlap the one already in flight.
    pub fn refresh(&mut self) {
        if self.pending_since.is_none() {
            self.next_probe = Instant::now();
        }
    }

    fn record(&mut self, report: Probe) {
        if self.history.len() == MAX_HISTORY {
            self.history.pop_front();
        }
        self.history
            .push_back((report.api.outcome.reachable(), report.site.outcome.reachable()));
        self.pending_since = None;
        self.next_probe = Instant::now() + PROBE_INTERVAL;
        self.last = Some(report);
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(report) = self.rx.try_recv() {
            self.record(report);
        }

        // If a worker panics or stalls, don't display an eternal "checking".
        if self
            .pending_since
            .is_some_and(|start| start.elapsed() > PROBE_TIMEOUT + Duration::from_secs(3))
        {
            self.record(Probe {
                api: Check::timed_out(),
                site: Check::timed_out(),
                measured_at: Instant::now(),
            });
        }

        if self.pending_since.is_none() && Instant::now() >= self.next_probe {
            let tx = self.tx.clone();
            let ctx = ctx.clone();
            match thread::Builder::new()
                .name("chatarium-https-probe".to_owned())
                .spawn(move || {
                    let report = check_routes();
                    let _ = tx.send(report);
                    ctx.request_repaint();
                }) {
                Ok(_) => self.pending_since = Some(Instant::now()),
                Err(error) => self.record(Probe {
                    api: Check {
                        outcome: Outcome::Unavailable(format!("Probe unavailable: {error}")),
                        elapsed_ms: None,
                    },
                    site: Check::timed_out(),
                    measured_at: Instant::now(),
                }),
            }
        }
    }

    /// The green authentication badge may coexist with a failed API route.
    /// This is *not* a claim that all internet access is down.
    pub fn is_offline(&self) -> bool {
        self.last.as_ref().is_some_and(|probe| {
            probe.measured_at.elapsed() <= MAX_PROBE_AGE
                && matches!(probe.api.outcome, Outcome::Failed(_) | Outcome::Unavailable(_))
        })
    }

    pub fn display(&self) -> Display {
        let Some(probe) = &self.last else {
            return Display {
                label: "NETWORK: checking API and website…".to_owned(),
                color: egui::Color32::from_rgb(225, 194, 108),
                tooltip: "Testing api.openai.com and chatgpt.com separately. No model request or credentials are sent.".to_owned(),
            };
        };
        let age = probe.measured_at.elapsed();
        if age > MAX_PROBE_AGE {
            return Display {
                label: format!("NETWORK: STALE · last check {}s ago", age.as_secs()),
                color: egui::Color32::from_rgb(225, 194, 108),
                tooltip: "The last API/website check is too old to trust.".to_owned(),
            };
        }
        let total = self.history.len();
        let api_ok = self.history.iter().filter(|(api, _)| *api).count();
        let site_ok = self.history.iter().filter(|(_, site)| *site).count();
        let api_unavailable = matches!(
            probe.api.outcome,
            Outcome::Failed(_) | Outcome::Unavailable(_)
        );
        let api_server_error = matches!(probe.api.outcome, Outcome::Http(500..=599));
        let api_unstable = total >= 3 && api_ok + 1 < total;
        let site_unstable = total >= 3 && site_ok + 1 < total;
        let color = if api_unavailable || probe.api.severe() {
            egui::Color32::from_rgb(238, 119, 119)
        } else if api_server_error
            || !probe.site.outcome.reachable()
            || probe.api.slow()
            || probe.site.slow()
            || api_unstable
            || site_unstable
        {
            egui::Color32::from_rgb(225, 194, 108)
        } else {
            egui::Color32::from_rgb(126, 210, 156)
        };
        Display {
            label: format!(
                "API {} {} {api_ok}/{total} · WEB {} {} {site_ok}/{total} · {}s ago",
                probe.api.label(),
                probe.api.time_label(),
                probe.site.label(),
                probe.site.time_label(),
                age.as_secs()
            ),
            color,
            tooltip: format!(
                "API: HEAD {API_URL} — {}. WEB: HEAD {SITE_URL} — {}. Full HTTPS request times (DNS, TCP, TLS, server response), NOT ICMP ping or bandwidth. Checks ~20s apart with 6s timeout. An HTTP 401/403/405 means the host responded, not that you are authenticated or able to submit a model request. Requests use a different method, authentication, and long-lived streaming transport. No credentials or model messages are sent.",
                probe.api.outcome.detail(),
                probe.site.outcome.detail()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(outcome: Outcome, ms: u64) -> Check {
        Check {
            outcome,
            elapsed_ms: Some(ms),
        }
    }

    fn probe(api: Outcome, site: Outcome, ms: u64) -> Probe {
        Probe {
            api: check(api, ms),
            site: check(site, ms),
            measured_at: Instant::now(),
        }
    }

    #[test]
    fn http_unauthorized_proves_reachability_not_authentication() {
        let output = std::process::Output {
            status: Command::new("rustc").arg("--version").status().unwrap(),
            stdout: b"401".to_vec(),
            stderr: Vec::new(),
        };
        assert_eq!(
            parse_curl_result(&output, Duration::from_millis(100)),
            Outcome::Http(401)
        );
        assert!(Outcome::Http(401).reachable());
        assert!(!Outcome::Http(503).reachable());
    }

    #[test]
    fn api_down_but_website_up_is_visible_and_red() {
        let mut state = NetworkHealth::default();
        state.record(probe(
            Outcome::Failed("API timed out".to_owned()),
            Outcome::Http(200),
            500,
        ));
        assert!(state.is_offline());
        let display = state.display();
        assert!(display.label.contains("API DOWN"));
        assert!(display.label.contains("WEB REACHED"));
        assert_eq!(display.color, egui::Color32::from_rgb(238, 119, 119));
    }

    #[test]
    fn expected_http_401_is_green_when_fast_and_reliable() {
        let mut state = NetworkHealth::default();
        state.record(probe(Outcome::Http(401), Outcome::Http(403), 250));
        assert!(!state.is_offline());
        assert_eq!(
            state.display().color,
            egui::Color32::from_rgb(126, 210, 156)
        );
    }

    #[test]
    fn severe_api_latency_is_red_and_slow_latency_is_amber() {
        let mut state = NetworkHealth::default();
        state.record(probe(Outcome::Http(401), Outcome::Http(200), 2_000));
        assert_eq!(
            state.display().color,
            egui::Color32::from_rgb(225, 194, 108)
        );
        state.record(probe(Outcome::Http(401), Outcome::Http(200), 4_200));
        assert_eq!(
            state.display().color,
            egui::Color32::from_rgb(238, 119, 119)
        );
    }

    #[test]
    fn stale_or_unmeasured_is_not_reported_as_connected() {
        let mut state = NetworkHealth::default();
        assert!(state.display().label.contains("checking"));
        let mut old = probe(Outcome::Http(401), Outcome::Http(200), 90);
        old.measured_at = Instant::now() - MAX_PROBE_AGE - Duration::from_secs(1);
        state.last = Some(old);
        assert!(state.display().label.contains("STALE"));
    }
}
