//! TUI event handling.
//!
//! This module provides event handling infrastructure for the TUI.
//! Some variants (Mouse, Resize) and methods (try_next) are intentionally
//! reserved for future mouse support and non-blocking event handling.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event as CrosstermEvent, KeyEvent, MouseEvent};

/// Application events.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum Event {
    /// Terminal tick (for animations/updates).
    Tick,
    /// Key press event.
    Key(KeyEvent),
    /// Mouse event.
    Mouse(MouseEvent),
    /// Terminal resize.
    Resize(u16, u16),
}

/// Event handler using channels.
pub struct EventHandler {
    /// Event receiver.
    rx: mpsc::Receiver<Event>,
    /// Sender (kept for cloning).
    _tx: mpsc::Sender<Event>,
}

impl EventHandler {
    /// Create a new event handler.
    pub fn new(tick_rate: Duration) -> Self {
        let (tx, rx) = mpsc::channel();
        let event_tx = tx.clone();

        // Spawn event loop thread
        thread::spawn(move || {
            loop {
                // Poll for events
                if event::poll(tick_rate).unwrap_or(false) {
                    match event::read() {
                        Ok(CrosstermEvent::Key(key)) => {
                            if event_tx.send(Event::Key(key)).is_err() {
                                break;
                            }
                        }
                        Ok(CrosstermEvent::Mouse(mouse)) => {
                            if event_tx.send(Event::Mouse(mouse)).is_err() {
                                break;
                            }
                        }
                        Ok(CrosstermEvent::Resize(w, h)) => {
                            if event_tx.send(Event::Resize(w, h)).is_err() {
                                break;
                            }
                        }
                        _ => {}
                    }
                }

                // Send tick event
                if event_tx.send(Event::Tick).is_err() {
                    break;
                }
            }
        });

        Self { rx, _tx: tx }
    }

    /// Get the next event.
    pub fn next(&self) -> Result<Event, mpsc::RecvError> {
        self.rx.recv()
    }

    /// Try to get the next event without blocking.
    ///
    /// This method is provided for non-blocking event handling scenarios,
    /// such as animation loops or when integrating with async code.
    #[allow(dead_code)]
    pub fn try_next(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }
}

/// Key binding configuration.
#[derive(Debug, Clone)]
pub struct KeyBindings {
    /// Quit keys.
    pub quit: Vec<KeyEvent>,
    /// Navigation up.
    pub up: Vec<KeyEvent>,
    /// Navigation down.
    pub down: Vec<KeyEvent>,
    /// Navigation left.
    pub left: Vec<KeyEvent>,
    /// Navigation right.
    pub right: Vec<KeyEvent>,
    /// Select/confirm.
    pub select: Vec<KeyEvent>,
    /// Back/cancel.
    pub back: Vec<KeyEvent>,
}

impl Default for KeyBindings {
    fn default() -> Self {
        use crossterm::event::{KeyCode, KeyModifiers};

        Self {
            quit: vec![
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            ],
            up: vec![
                KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
            ],
            down: vec![
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            ],
            left: vec![
                KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
            ],
            right: vec![
                KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
            ],
            select: vec![KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)],
            back: vec![KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)],
        }
    }
}

impl KeyBindings {
    /// Check if a key matches quit binding.
    pub fn is_quit(&self, key: &KeyEvent) -> bool {
        self.quit.iter().any(|k| k.code == key.code && k.modifiers == key.modifiers)
    }

    /// Check if a key matches up binding.
    pub fn is_up(&self, key: &KeyEvent) -> bool {
        self.up.iter().any(|k| k.code == key.code && k.modifiers == key.modifiers)
    }

    /// Check if a key matches down binding.
    pub fn is_down(&self, key: &KeyEvent) -> bool {
        self.down.iter().any(|k| k.code == key.code && k.modifiers == key.modifiers)
    }

    /// Check if a key matches left binding.
    pub fn is_left(&self, key: &KeyEvent) -> bool {
        self.left.iter().any(|k| k.code == key.code && k.modifiers == key.modifiers)
    }

    /// Check if a key matches right binding.
    pub fn is_right(&self, key: &KeyEvent) -> bool {
        self.right.iter().any(|k| k.code == key.code && k.modifiers == key.modifiers)
    }

    /// Check if a key matches select binding.
    pub fn is_select(&self, key: &KeyEvent) -> bool {
        self.select.iter().any(|k| k.code == key.code && k.modifiers == key.modifiers)
    }

    /// Check if a key matches back binding.
    pub fn is_back(&self, key: &KeyEvent) -> bool {
        self.back.iter().any(|k| k.code == key.code && k.modifiers == key.modifiers)
    }
}
