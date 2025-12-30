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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    mod key_bindings {
        use super::*;

        #[test]
        fn test_default_bindings() {
            let bindings = KeyBindings::default();

            // Should have quit bindings for 'q' and Ctrl+C
            assert_eq!(bindings.quit.len(), 2);

            // Should have up bindings for Up arrow and 'k'
            assert_eq!(bindings.up.len(), 2);

            // Should have down bindings for Down arrow and 'j'
            assert_eq!(bindings.down.len(), 2);

            // Should have left bindings for Left arrow and 'h'
            assert_eq!(bindings.left.len(), 2);

            // Should have right bindings for Right arrow and 'l'
            assert_eq!(bindings.right.len(), 2);

            // Should have Enter for select
            assert_eq!(bindings.select.len(), 1);

            // Should have Esc for back
            assert_eq!(bindings.back.len(), 1);
        }

        #[test]
        fn test_is_quit_q_key() {
            let bindings = KeyBindings::default();
            let q_key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
            assert!(bindings.is_quit(&q_key));
        }

        #[test]
        fn test_is_quit_ctrl_c() {
            let bindings = KeyBindings::default();
            let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert!(bindings.is_quit(&ctrl_c));
        }

        #[test]
        fn test_is_quit_false_for_other_keys() {
            let bindings = KeyBindings::default();
            let x_key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
            assert!(!bindings.is_quit(&x_key));
        }

        #[test]
        fn test_is_up_arrow() {
            let bindings = KeyBindings::default();
            let up_arrow = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
            assert!(bindings.is_up(&up_arrow));
        }

        #[test]
        fn test_is_up_k_key() {
            let bindings = KeyBindings::default();
            let k_key = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
            assert!(bindings.is_up(&k_key));
        }

        #[test]
        fn test_is_down_arrow() {
            let bindings = KeyBindings::default();
            let down_arrow = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
            assert!(bindings.is_down(&down_arrow));
        }

        #[test]
        fn test_is_down_j_key() {
            let bindings = KeyBindings::default();
            let j_key = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
            assert!(bindings.is_down(&j_key));
        }

        #[test]
        fn test_is_left_arrow() {
            let bindings = KeyBindings::default();
            let left_arrow = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
            assert!(bindings.is_left(&left_arrow));
        }

        #[test]
        fn test_is_left_h_key() {
            let bindings = KeyBindings::default();
            let h_key = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
            assert!(bindings.is_left(&h_key));
        }

        #[test]
        fn test_is_right_arrow() {
            let bindings = KeyBindings::default();
            let right_arrow = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
            assert!(bindings.is_right(&right_arrow));
        }

        #[test]
        fn test_is_right_l_key() {
            let bindings = KeyBindings::default();
            let l_key = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE);
            assert!(bindings.is_right(&l_key));
        }

        #[test]
        fn test_is_select_enter() {
            let bindings = KeyBindings::default();
            let enter_key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
            assert!(bindings.is_select(&enter_key));
        }

        #[test]
        fn test_is_back_escape() {
            let bindings = KeyBindings::default();
            let esc_key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
            assert!(bindings.is_back(&esc_key));
        }

        #[test]
        fn test_modifier_sensitivity() {
            let bindings = KeyBindings::default();

            // 'q' with no modifiers should quit
            let q_plain = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
            assert!(bindings.is_quit(&q_plain));

            // 'q' with SHIFT should NOT quit (different modifier)
            let q_shift = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::SHIFT);
            assert!(!bindings.is_quit(&q_shift));

            // 'c' alone should NOT quit (needs CONTROL)
            let c_plain = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
            assert!(!bindings.is_quit(&c_plain));
        }

        #[test]
        fn test_vim_navigation_complete() {
            let bindings = KeyBindings::default();

            // h, j, k, l should map to left, down, up, right respectively
            let h = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
            let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
            let k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
            let l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE);

            assert!(bindings.is_left(&h));
            assert!(bindings.is_down(&j));
            assert!(bindings.is_up(&k));
            assert!(bindings.is_right(&l));

            // And they should NOT match other directions
            assert!(!bindings.is_right(&h));
            assert!(!bindings.is_up(&j));
            assert!(!bindings.is_down(&k));
            assert!(!bindings.is_left(&l));
        }
    }

    mod event_enum {
        use super::*;

        #[test]
        fn test_event_tick() {
            let event = Event::Tick;
            assert!(matches!(event, Event::Tick));
        }

        #[test]
        fn test_event_key() {
            let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
            let event = Event::Key(key);
            assert!(matches!(event, Event::Key(_)));
        }

        #[test]
        fn test_event_resize() {
            let event = Event::Resize(80, 24);
            if let Event::Resize(w, h) = event {
                assert_eq!(w, 80);
                assert_eq!(h, 24);
            } else {
                panic!("Expected Resize event");
            }
        }

        #[test]
        fn test_event_clone() {
            let event = Event::Tick;
            let cloned = event.clone();
            assert!(matches!(cloned, Event::Tick));
        }

        #[test]
        fn test_event_debug() {
            let event = Event::Tick;
            let debug_str = format!("{:?}", event);
            assert!(debug_str.contains("Tick"));
        }
    }
}
