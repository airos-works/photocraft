//! Frame information supplied by the platform host. The editor does not own a window.
#[cfg(not(target_os = "haiku"))]
pub use eframe::Frame;

#[cfg(target_os = "haiku")]
#[derive(Default)]
pub struct Frame {
    pub cpu_usage: Option<f32>,
}
#[cfg(target_os = "haiku")]
impl Frame {
    pub fn info(&self) -> &Self {
        self
    }
}
