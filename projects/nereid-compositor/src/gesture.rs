//! Touch gesture recognition for edge swipes.
//!
//! Detects swipe gestures from screen edges to show/hide layer surfaces:
//! - Swipe down from top edge → quick-panel
//! - Swipe up from bottom edge → notifications
//! - Swipe left/right → space switching (future)
//!
//! State machine:
//! Idle → MaybeGesture (touch starts in edge zone)
//!      → Gesture (drag distance exceeds threshold) → fires action
//!      → PassThrough (drag not in gesture direction) → forwards touch to client

/// Edge zone width in pixels from screen border.
const EDGE_ZONE: f64 = 50.0;

/// Minimum drag distance (pixels) to confirm a gesture.
const GESTURE_THRESHOLD: f64 = 60.0;

/// Which edge a gesture originated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// Result of processing a touch event through the gesture recognizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureResult {
    /// Touch is part of a gesture — do NOT forward to clients.
    Consumed,
    /// Touch should be forwarded to the focused client as normal.
    Forward,
    /// A gesture was completed.
    Completed(GestureAction),
}

/// Action triggered by a completed gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureAction {
    /// Swipe down from top → toggle quick-panel.
    SwipeDown,
    /// Swipe up from bottom → toggle notifications.
    SwipeUp,
    /// Swipe from left edge.
    SwipeLeft,
    /// Swipe from right edge.
    SwipeRight,
}

#[derive(Debug)]
enum State {
    Idle,
    /// Touch started in an edge zone, waiting to see if it's a gesture.
    MaybeGesture {
        edge: Edge,
        start_x: f64,
        start_y: f64,
    },
    /// Confirmed gesture in progress.
    Gesture { edge: Edge },
    /// Not a gesture — all touch events pass through to client.
    PassThrough,
}

pub struct GestureRecognizer {
    state: State,
    screen_width: f64,
    screen_height: f64,
}

impl GestureRecognizer {
    pub fn new(screen_width: u32, screen_height: u32) -> Self {
        Self {
            state: State::Idle,
            screen_width: screen_width as f64,
            screen_height: screen_height as f64,
        }
    }

    /// Process a touch down event. Returns whether to forward it.
    pub fn touch_down(&mut self, x: f64, y: f64) -> GestureResult {
        // Only track slot 0 for gestures
        let edge = self.detect_edge(x, y);
        match edge {
            Some(edge) => {
                self.state = State::MaybeGesture {
                    edge,
                    start_x: x,
                    start_y: y,
                };
                // Don't forward yet — wait to see if it's a gesture
                GestureResult::Consumed
            }
            None => {
                self.state = State::PassThrough;
                GestureResult::Forward
            }
        }
    }

    /// Process a touch motion event.
    pub fn touch_motion(&mut self, x: f64, y: f64) -> GestureResult {
        match self.state {
            State::MaybeGesture {
                edge,
                start_x,
                start_y,
            } => {
                let dx = x - start_x;
                let dy = y - start_y;

                // Check if drag is in the gesture direction
                let (distance, is_correct_direction) = match edge {
                    Edge::Top => (dy, dy > 0.0),     // swipe down
                    Edge::Bottom => (-dy, dy < 0.0),  // swipe up
                    Edge::Left => (dx, dx > 0.0),     // swipe right
                    Edge::Right => (-dx, dx < 0.0),   // swipe left
                };

                if !is_correct_direction && distance.abs() > GESTURE_THRESHOLD / 2.0 {
                    // Dragging wrong way — not a gesture
                    self.state = State::PassThrough;
                    return GestureResult::Forward;
                }

                if distance > GESTURE_THRESHOLD {
                    self.state = State::Gesture { edge };
                    return GestureResult::Consumed;
                }

                GestureResult::Consumed
            }
            State::Gesture { .. } => GestureResult::Consumed,
            State::PassThrough => GestureResult::Forward,
            State::Idle => GestureResult::Forward,
        }
    }

    /// Process a touch up event.
    pub fn touch_up(&mut self) -> GestureResult {
        let result = match self.state {
            State::Gesture { edge } => {
                let action = match edge {
                    Edge::Top => GestureAction::SwipeDown,
                    Edge::Bottom => GestureAction::SwipeUp,
                    Edge::Left => GestureAction::SwipeRight,
                    Edge::Right => GestureAction::SwipeLeft,
                };
                GestureResult::Completed(action)
            }
            State::MaybeGesture { .. } => {
                // Touch started in edge but didn't move enough — treat as a tap, forward it
                GestureResult::Forward
            }
            State::PassThrough => GestureResult::Forward,
            State::Idle => GestureResult::Forward,
        };
        self.state = State::Idle;
        result
    }

    fn detect_edge(&self, x: f64, y: f64) -> Option<Edge> {
        if y < EDGE_ZONE {
            Some(Edge::Top)
        } else if y > self.screen_height - EDGE_ZONE {
            Some(Edge::Bottom)
        } else if x < EDGE_ZONE {
            Some(Edge::Left)
        } else if x > self.screen_width - EDGE_ZONE {
            Some(Edge::Right)
        } else {
            None
        }
    }
}
