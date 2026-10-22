//! Hierarchy and inspector state: pickers, refresh, and entity edits.
use super::*;

impl Ui {
    pub(super) fn fill_picker(&self, id: usize, kind: &str, selected: Option<&str>) {
        let picker = self.control(id);
        // SAFETY: Combo box messages are synchronous and string data is copied.
        unsafe {
            SendMessageW(picker, CB_RESETCONTENT, 0, 0);
            SendMessageW(picker, CB_ADDSTRING, 0, wide("(none)").as_ptr() as LPARAM);
            SendMessageW(picker, CB_SETCURSEL, 0, 0);
            if let Some(document) = self.controller.borrow().document() {
                for (index, asset) in document
                    .project()
                    .assets
                    .values()
                    .filter(|a| a.kind == kind)
                    .enumerate()
                {
                    SendMessageW(
                        picker,
                        CB_ADDSTRING,
                        0,
                        wide(&format!("{} | {}", asset.id, asset.path.display())).as_ptr()
                            as LPARAM,
                    );
                    if selected == Some(asset.id.as_str()) {
                        SendMessageW(picker, CB_SETCURSEL, index + 1, 0);
                    }
                }
            }
        }
    }

    pub(super) fn selected_asset(&self, id: usize, kind: &str) -> Option<String> {
        // SAFETY: Message targets a live combo box on this thread.
        let index = unsafe { SendMessageW(self.control(id), CB_GETCURSEL, 0, 0) };
        if index <= 0 {
            return None;
        }
        self.controller
            .borrow()
            .document()?
            .project()
            .assets
            .values()
            .filter(|a| a.kind == kind)
            .nth(index as usize - 1)
            .map(|a| a.id.clone())
    }

    pub(super) fn refresh(&self) {
        self.updating.set(true);
        {
            let controller = self.controller.borrow();
            let title = format!(
                "Rust Game Engine Editor{}",
                if controller.is_dirty() { " *" } else { "" }
            );
            set_text(self.window.get(), &title);
            set_text(self.control(STATUS), controller.status());
            let lines = controller
                .diagnostics()
                .iter()
                .chain(controller.log().iter())
                .cloned()
                .collect::<Vec<_>>()
                .join("\r\n");
            set_text(self.control(LOG), &lines);
            // SAFETY: All IDs below are live child controls; only copied strings
            // and scalar selection indices cross this synchronous message boundary.
            unsafe {
                SendMessageW(self.control(ENTITIES), LB_RESETCONTENT, 0, 0);
                if let Some(document) = controller.document() {
                    for (index, entity) in document.snapshot().entities.iter().enumerate() {
                        SendMessageW(
                            self.control(ENTITIES),
                            LB_ADDSTRING,
                            0,
                            wide(&format!("{} | {}", entity.name, entity.id)).as_ptr() as LPARAM,
                        );
                        if controller.selected() == Some(entity.id.as_str()) {
                            SendMessageW(self.control(ENTITIES), LB_SETCURSEL, index, 0);
                        }
                    }
                }
                for id in [SAVE, RELOAD, UNDO, REDO] {
                    EnableWindow(self.control(id), i32::from(controller.document().is_some()));
                }
                EnableWindow(
                    self.control(PLAY),
                    i32::from(controller.document().is_some() && !controller.is_playing()),
                );
                EnableWindow(self.control(STOP), i32::from(controller.is_playing()));
            }
        }
        let entity = {
            let controller = self.controller.borrow();
            controller.document().and_then(|document| {
                document
                    .snapshot()
                    .entities
                    .iter()
                    .find(|e| Some(e.id.as_str()) == controller.selected())
                    .cloned()
            })
        };
        let position = entity
            .as_ref()
            .and_then(|e| e.components.transform.as_ref())
            .map(|t| t.position)
            .unwrap_or([0.0; 3]);
        for (id, value) in [POS_X, POS_Y, POS_Z].into_iter().zip(position) {
            set_text(self.control(id), &value.to_string());
        }
        self.fill_picker(
            SCRIPT,
            "script",
            entity
                .as_ref()
                .and_then(|e| e.components.script.as_ref())
                .map(|s| s.asset.as_str()),
        );
        self.fill_picker(
            MESH,
            "mesh",
            entity
                .as_ref()
                .and_then(|e| e.components.mesh_renderer.as_ref())
                .map(|m| m.mesh.as_str()),
        );
        self.fill_picker(
            TEXTURE,
            "texture",
            entity
                .as_ref()
                .and_then(|e| e.components.mesh_renderer.as_ref())
                .and_then(|m| m.texture.as_deref()),
        );
        // SAFETY: Controls exist until the parent window is destroyed.
        unsafe {
            for id in [POS_X, POS_Y, POS_Z, APPLY_POSITION, SCRIPT, MESH] {
                EnableWindow(self.control(id), i32::from(entity.is_some()));
            }
            EnableWindow(
                self.control(TEXTURE),
                i32::from(
                    entity
                        .as_ref()
                        .is_some_and(|e| e.components.mesh_renderer.is_some()),
                ),
            );
        }
        self.updating.set(false);
    }
    pub(super) fn edit(&self, id: usize) -> Result<()> {
        let (entity_id, revision) = {
            let controller = self.controller.borrow();
            (
                controller
                    .selected()
                    .ok_or_else(|| anyhow!("select an entity first"))?
                    .to_owned(),
                controller
                    .document()
                    .ok_or_else(|| anyhow!("no project open"))?
                    .revision()
                    .to_owned(),
            )
        };
        let operation = match id {
            APPLY_POSITION => {
                let mut position = [0.0; 3];
                for (value, control) in position.iter_mut().zip([POS_X, POS_Y, POS_Z]) {
                    *value = text(self.control(control))
                        .trim()
                        .parse::<f32>()
                        .map_err(|_| anyhow!("position must contain finite numbers"))?;
                    ensure!(value.is_finite(), "position must contain finite numbers");
                }
                EditOperation::SetPosition {
                    entity_id,
                    position,
                }
            }
            SCRIPT => EditOperation::SetScript {
                entity_id,
                asset: self.selected_asset(SCRIPT, "script"),
            },
            MESH | TEXTURE => {
                let mesh = self.selected_asset(MESH, "mesh");
                let texture = self.selected_asset(TEXTURE, "texture");
                EditOperation::SetMeshRenderer {
                    entity_id,
                    value: mesh.map(|mesh| MeshRenderer {
                        version: 1,
                        mesh,
                        texture,
                    }),
                }
            }
            _ => bail!("unknown inspector field"),
        };
        self.controller.borrow_mut().apply(operation, &revision)
    }
}
