//! Printer selection controls and their transient state.
//!
//! The host supplies two workers through channels: discovery (a list of printers) and
//! submission (a receipt or an error). This module only tracks what the user picked and what the
//! workers reported; it never talks to a spooler or touches a PDF, so it works on every target,
//! WASM included. Hosts map [`Settings`] onto their print backend's job type.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use crate::wake::{self, Reason};

/// A printer as reported by the host's discovery worker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Printer {
    pub name: String,
    pub default: bool,
}

/// Two-sided printing choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Duplex {
    Off,
    LongEdge,
    ShortEdge,
}

/// What the user chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Name of the chosen printer; empty while none is chosen.
    pub printer: String,
    pub copies: u32,
    /// `None` keeps the printer's own default.
    pub duplex: Option<Duplex>,
    pub grayscale: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { printer: String::new(), copies: 1, duplex: None, grayscale: false }
    }
}

/// Starts printer discovery and returns where the list will arrive.
pub type Discover = Box<dyn Fn() -> Receiver<Vec<Printer>>>;
/// Where a submission's receipt (or error text) arrives.
pub type Submission = Receiver<Result<String, String>>;
/// Starts a submission; a convenience for hosts (this module never calls it).
pub type Submit = Box<dyn Fn(Vec<u8>, Settings) -> Submission>;

/// The latest thing worth telling the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Feedback {
    FindingPrinters,
    NoPrinters,
    DiscoveryFailed,
    Submitting,
    /// The worker's receipt, unchanged.
    Submitted(String),
    /// The worker's error text, unchanged.
    Failed(String),
    /// The submission worker went away without answering.
    WorkerStopped,
}

/// Localized texts for [`State::show`].
pub struct Labels<'a> {
    pub printer: &'a str,
    pub select_printer: &'a str,
    pub refresh: &'a str,
    pub copies: &'a str,
    pub printer_default: &'a str,
    pub duplex: &'a str,
    pub one_sided: &'a str,
    pub long_edge: &'a str,
    pub short_edge: &'a str,
    pub grayscale: &'a str,
}

/// Print settings plus whatever the workers are doing right now. Not meant to be persisted.
#[derive(Default)]
pub struct State {
    pub settings: Settings,
    pub printers: Vec<Printer>,
    pub message: Option<Feedback>,
    discovery: Option<Receiver<Vec<Printer>>>,
    submission: Option<Submission>,
    loaded: bool,
}

/// How often a pending worker is checked.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

impl State {
    /// A submission is in flight.
    pub fn busy(&self) -> bool {
        self.submission.is_some()
    }

    /// Discovery is in flight.
    pub fn discovering(&self) -> bool {
        self.discovery.is_some()
    }

    /// Nothing is pending and the chosen printer is in the current list.
    pub fn ready(&self) -> bool {
        !self.busy() && !self.discovering() && self.printers.iter().any(|p| p.name == self.settings.printer)
    }

    /// Track a submission the host started. Refused (the receiver comes back) while another
    /// one is in flight. Dropping a receiver does not cancel the host's job.
    pub fn begin(&mut self, receiver: Submission) -> Result<(), Submission> {
        if self.busy() {
            return Err(receiver);
        }
        self.submission = Some(receiver);
        self.message = Some(Feedback::Submitting);
        Ok(())
    }

    /// Start discovery, unless discovery or a submission is already running.
    pub fn refresh(&mut self, discover: &Discover) -> bool {
        if self.discovering() || self.busy() {
            return false;
        }
        self.discovery = Some(discover());
        self.loaded = true;
        self.message = Some(Feedback::FindingPrinters);
        true
    }

    /// Take in whatever the workers have sent, without blocking. Schedules another frame while
    /// something is still pending.
    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(receiver) = &self.discovery {
            match receiver.try_recv() {
                Ok(list) => {
                    self.discovery = None;
                    self.take_printers(list);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.discovery = None;
                    self.printers.clear();
                    self.message = Some(Feedback::DiscoveryFailed);
                }
            }
        }
        if let Some(receiver) = &self.submission {
            let outcome = match receiver.try_recv() {
                Ok(Ok(receipt)) => Some(Feedback::Submitted(receipt)),
                Ok(Err(error)) => Some(Feedback::Failed(error)),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Feedback::WorkerStopped),
            };
            if let Some(outcome) = outcome {
                self.submission = None;
                self.message = Some(outcome);
            }
        }
        if self.discovering() || self.busy() {
            wake::wake_after(ctx, Reason::JOB, POLL_INTERVAL);
        }
    }

    /// A fresh list: keep the chosen printer if it survived, else pick the default, else the
    /// first, else nothing.
    fn take_printers(&mut self, list: Vec<Printer>) {
        self.printers = list;
        if !self.printers.iter().any(|p| p.name == self.settings.printer) {
            let pick = self.printers.iter().find(|p| p.default).or_else(|| self.printers.first());
            self.settings.printer = pick.map(|p| p.name.clone()).unwrap_or_default();
        }
        self.message = self.printers.is_empty().then_some(Feedback::NoPrinters);
    }

    /// Draw the controls under `id`: printer, refresh, copies, duplex and grayscale. The first
    /// call starts discovery. Everything is disabled while a submission is in flight. This never
    /// submits anything.
    pub fn show(&mut self, ui: &mut egui::Ui, id: egui::Id, discover: &Discover, labels: &Labels<'_>) {
        if !self.loaded {
            self.refresh(discover);
        }
        self.poll(ui.ctx());
        let busy = self.busy();
        ui.push_id(id, |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                let width = ui.available_width().clamp(32.0, 230.0);

                let printer_label = ui.label(labels.printer);
                let current = if self.settings.printer.is_empty() { labels.select_printer.to_string() } else { self.settings.printer.clone() };
                let printers = &self.printers;
                let chosen = &mut self.settings.printer;
                let combo = egui::ComboBox::from_id_salt("printer").width(width).truncate().selected_text(current).show_ui(ui, |ui| {
                    for p in printers {
                        ui.selectable_value(chosen, p.name.clone(), p.name.as_str());
                    }
                });
                combo.response.labelled_by(printer_label.id);

                let discovering = self.discovering();
                if ui.add_enabled(!discovering, egui::Button::new(labels.refresh)).clicked() {
                    self.refresh(discover);
                }

                ui.horizontal(|ui| {
                    let copies_label = ui.label(labels.copies);
                    ui.add(egui::DragValue::new(&mut self.settings.copies).range(1..=999)).labelled_by(copies_label.id);
                });

                let duplex_label = ui.label(labels.duplex);
                let choices = [
                    (None, labels.printer_default),
                    (Some(Duplex::Off), labels.one_sided),
                    (Some(Duplex::LongEdge), labels.long_edge),
                    (Some(Duplex::ShortEdge), labels.short_edge),
                ];
                let shown = choices.iter().find(|(value, _)| *value == self.settings.duplex).map_or(labels.printer_default, |(_, text)| text);
                let duplex = &mut self.settings.duplex;
                let combo = egui::ComboBox::from_id_salt("duplex").width(width).truncate().selected_text(shown).show_ui(ui, |ui| {
                    for (value, text) in choices {
                        ui.selectable_value(duplex, value, text);
                    }
                });
                combo.response.labelled_by(duplex_label.id);

                ui.checkbox(&mut self.settings.grayscale, labels.grayscale);
            });
        });
    }
}

#[cfg(test)]
mod tests;
