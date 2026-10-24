//! The `render_screenshot` tool.

use super::*;
use super::{build::*, exec::*};
use crate::arguments::{DEFAULT_SCREENSHOT_SIZE, MAX_SCREENSHOT_EDGE};
use base64::{Engine, engine::general_purpose::STANDARD};
use engine_mcp::{
    AccessLevel, Annotations, Content, Tool, ToolContext, ToolDefinition, ToolFailure, ToolOutcome,
    ToolResult,
    schema::{self, Args},
};
use serde_json::{Map, Value, json};
use std::{sync::Arc, time::Instant};

pub(super) struct ScreenshotTool(pub(super) Arc<State>);

impl Tool for ScreenshotTool {
    fn definition(&self) -> ToolDefinition {
        let mut properties = vec![
            (
                "scene",
                schema::string("Project-relative scene to render instead of the startup scene."),
            ),
            (
                "ticks",
                schema::integer(
                    "Fixed 1/60 s ticks to simulate before capturing (1 shows the initial state).",
                    1,
                    MAX_SCREENSHOT_TICKS as i64,
                    Some(1),
                ),
            ),
            ("inputs", inputs_schema()),
            (
                "width",
                schema::integer(
                    "Image width in pixels.",
                    16,
                    2048,
                    Some(DEFAULT_SCREENSHOT_SIZE[0] as i64),
                ),
            ),
            (
                "height",
                schema::integer(
                    "Image height in pixels.",
                    16,
                    2048,
                    Some(DEFAULT_SCREENSHOT_SIZE[1] as i64),
                ),
            ),
        ];
        properties.extend(timeout_properties());
        ToolDefinition {
            name: "render_screenshot".into(),
            title: "Render screenshot".into(),
            description:
                "Build the project (unless rebuild is false), simulate N fixed ticks with \
                optional scripted input and render the final frame offscreen to a PNG, returned as \
                an MCP image plus metadata (size, GPU adapter, tick count). Uses the engine's real \
                renderer, so sprites, meshes and UI panels appear as in the game. Needs a GPU \
                adapter: on hosts without one it fails with the structured error `gpu_unavailable` \
                (use project_run_headless instead). Executes the project's native game code with \
                your privileges."
                    .into(),
            input_schema: schema::object(properties, &[]),
            output_schema: Some(schema::output(vec![
                ("width", json!({"type": "integer"})),
                ("height", json!({"type": "integer"})),
                ("ticks", json!({"type": "integer"})),
                ("png_bytes", json!({"type": "integer"})),
                ("scene_id", schema::string("Scene that was rendered.")),
                (
                    "adapter",
                    json!({"type": "object", "properties": {
                    "name": {"type": "string"}, "backend": {"type": "string"},
                    "device_type": {"type": "string"}}}),
                ),
                (
                    "upscaler",
                    json!({"type": ["object", "null"], "description":
                        "Temporal upscaler request and what the renderer did for this image."}),
                ),
                (
                    "frame_generation",
                    json!({"type": ["object", "null"], "description":
                        "Frame generation request and status; with dlss_fg the previous tick is \
                         rendered first so NGX can generate the frames before this image."}),
                ),
                (
                    "generated_frames",
                    json!({"type": "integer", "description":
                        "NGX-generated frames produced before the returned real frame."}),
                ),
                ("build", json!({"type": "object"})),
                ("run_millis", json!({"type": "integer"})),
            ])),
            annotations: Annotations {
                read_only: false,
                destructive: true,
                idempotent: false,
                open_world: true,
            },
            access: AccessLevel::Exec,
        }
    }

    fn call(&self, cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
        cx.require(AccessLevel::Exec)?;
        let a = Args(args);
        let ticks = a.integer("ticks", 1, MAX_SCREENSHOT_TICKS, 1)?;
        let width = a.integer("width", 16, 2048, DEFAULT_SCREENSHOT_SIZE[0] as u64)? as u32;
        let height = a.integer("height", 16, 2048, DEFAULT_SCREENSHOT_SIZE[1] as u64)? as u32;
        debug_assert!(width.max(height) <= MAX_SCREENSHOT_EDGE);
        let rebuild = a.boolean("rebuild", true)?;
        let (build_timeout, run_timeout) = timeouts(&a)?;
        let scene = scene_override(cx, &a)?;
        let project = cx.project()?;
        let inputs = parse_inputs(&project, &a, ticks)?;
        let built = obtain_generation(&self.0, &project, rebuild, false, build_timeout)?;
        let workspace =
            tempfile::tempdir().map_err(|e| ToolFailure::new("tooling_error", e.to_string()))?;
        let png_path = workspace.path().join("screenshot.png");
        let mut arguments = base_arguments(
            cx,
            ticks,
            &inputs,
            &workspace.path().join("user-data"),
            scene.as_ref(),
        );
        arguments.extend([
            "--screenshot".into(),
            png_path.clone().into(),
            "--screenshot-size".into(),
            format!("{width}x{height}").into(),
        ]);
        let build = build_json(cx, &built);
        let started = Instant::now();
        let report = execute(built.artifact, &arguments, run_timeout)?;
        let png = std::fs::read(&png_path).map_err(|_| {
            ToolFailure::new("run_no_result", "the run succeeded but produced no image")
        })?;
        let encoded = STANDARD.encode(&png);
        let budget = cx
            .limits
            .max_output_message_bytes
            .saturating_sub(IMAGE_OVERHEAD);
        if encoded.len() > budget {
            return Err(ToolFailure::new(
                "image_too_large",
                format!(
                    "the {width}x{height} image is {} KiB, above the {} KiB response budget",
                    encoded.len() / 1024,
                    budget / 1024
                ),
            )
            .with_hint("Request a smaller width and height (for example 320x180)."));
        }
        let shot = &report["screenshot"];
        let result = json!({
            "width": width, "height": height, "ticks": report["ticks"],
            "png_bytes": png.len(), "adapter": shot["adapter"],
            "scene_id": report["scene"]["scene"]["scene_id"],
            "upscaler": report["scene"]["upscaler"],
            "frame_generation": report["scene"]["frame_generation"],
            "generated_frames": shot["generated_frames"].as_array().map_or(0, Vec::len),
            "build": build, "run_millis": started.elapsed().as_millis(),
        });
        Ok(ToolResult::new(result).with_content(Content::Image {
            data: encoded,
            mime_type: "image/png".into(),
        }))
    }
}
