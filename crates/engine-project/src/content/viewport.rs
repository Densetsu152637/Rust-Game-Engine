#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
}
pub fn camera_viewport(logical: [u32; 2], framebuffer: [u32; 2]) -> Viewport {
    if logical.contains(&0) || framebuffer.contains(&0) {
        return Viewport {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
            scale: 0.0,
        };
    }
    let fit =
        (framebuffer[0] as f32 / logical[0] as f32).min(framebuffer[1] as f32 / logical[1] as f32);
    let scale = if fit >= 1.0 { fit.floor() } else { fit };
    let width = logical[0] as f32 * scale;
    let height = logical[1] as f32 * scale;
    Viewport {
        x: ((framebuffer[0] as f32 - width) / 2.0).floor(),
        y: ((framebuffer[1] as f32 - height) / 2.0).floor(),
        width,
        height,
        scale,
    }
}
pub fn logical_pointer(
    pointer: [f32; 2],
    window: [u32; 2],
    framebuffer: [u32; 2],
    logical: [u32; 2],
) -> [f32; 2] {
    let viewport = camera_viewport(logical, framebuffer);
    if window.contains(&0) || viewport.scale == 0.0 || !pointer.iter().all(|n| n.is_finite()) {
        return [-1.0; 2];
    }
    [
        (pointer[0] * framebuffer[0] as f32 / window[0] as f32 - viewport.x) / viewport.scale,
        (pointer[1] * framebuffer[1] as f32 / window[1] as f32 - viewport.y) / viewport.scale,
    ]
}
