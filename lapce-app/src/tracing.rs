// Re-export `tracing` crate under own name to not collide and as convenient import
pub use tracing::{self, event as trace, Instrument, Level as TraceLevel};
