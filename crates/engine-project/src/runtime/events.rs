use super::*;
use crate::events::{EngineEvent, EventBindings, EventHandlers, EventLimits};
impl Runtime {
    pub(super) fn validate_event_bindings(&self) -> Result<()> {
        for entity in self.app.world().iter_entities() {
            if let Some(bindings) = entity.get::<EventBindings>() {
                for binding in &bindings.bindings {
                    if !self
                        .app
                        .world()
                        .get_resource::<EventHandlers>()
                        .is_some_and(|h| h.0.contains_key(&binding.handler))
                    {
                        return Err(Error::new(
                            "event.handler.unregistered",
                            format!("No callback {}", binding.handler),
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    pub(super) fn dispatch_events(&mut self) -> Result<()> {
        let events = crate::events::take_events(self.app.world_mut());
        let limit = self
            .app
            .world()
            .get_resource::<EventLimits>()
            .copied()
            .unwrap_or_default()
            .deliveries_per_tick;
        let mut delivered = 0;
        for event in &events {
            let targets: Vec<_> = if let Some(target) = event.target {
                vec![target]
            } else {
                let mut targets: Vec<_> = self
                    .app
                    .world()
                    .iter_entities()
                    .filter(|e| e.contains::<EventBindings>() || e.contains::<Script>())
                    .map(|e| {
                        (
                            e.get::<AuthoredId>()
                                .map(|a| a.id.clone())
                                .unwrap_or_else(|| format!("{:?}", e.id())),
                            e.id(),
                        )
                    })
                    .collect();
                targets.sort_by(|a, b| a.0.cmp(&b.0));
                targets.into_iter().map(|(_, e)| e).collect()
            };
            for target in targets {
                if self.app.world().get_entity(target).is_err() {
                    continue;
                }
                let bindings = self
                    .app
                    .world()
                    .get::<EventBindings>(target)
                    .cloned()
                    .unwrap_or_default();
                for binding in bindings.bindings.iter().filter(|b| b.event == event.name) {
                    if self.app.world().get_entity(target).is_err() {
                        break;
                    }
                    delivered += 1;
                    if delivered > limit {
                        return Err(Error::new(
                            "event.delivery.limit",
                            "Per-tick event delivery limit exceeded",
                        ));
                    }
                    let handler = self
                        .app
                        .world()
                        .get_resource::<EventHandlers>()
                        .and_then(|h| h.0.get(&binding.handler))
                        .copied()
                        .ok_or_else(|| {
                            Error::new("event.handler.unregistered", "Missing event callback")
                        })?;
                    invoke_owned(self.app.world_mut(), target, |world| {
                        handler(world, target, event)
                    })?;
                }
                if self.app.world().get_entity(target).is_ok()
                    && let Some(index) = self.scripts.iter().position(|s| s.owner == target)
                {
                    delivered += 1;
                    if delivered > limit {
                        return Err(Error::new(
                            "event.delivery.limit",
                            "Per-tick event delivery limit exceeded",
                        ));
                    }
                    let script = &mut self.scripts[index];
                    invoke_owned(self.app.world_mut(), target, |world| {
                        script.script.on_event(world, target, event)
                    })?;
                }
            }
        }
        Ok(())
    }
    pub(super) fn queue_trigger_events(&mut self) -> Result<()> {
        let triggers = self.app.world().resource::<TriggerEvents>().0.clone();
        for trigger in triggers {
            let phase = match trigger.phase {
                gameplay2d::TriggerPhase::Enter => "enter",
                gameplay2d::TriggerPhase::Stay => "stay",
                gameplay2d::TriggerPhase::Exit => "exit",
            };
            for (target, other) in [
                (&trigger.first, &trigger.second),
                (&trigger.second, &trigger.first),
            ] {
                if let Some(target) = self.entity(target) {
                    let source = self.entity(other);
                    crate::emit_event(
                        self.app.world_mut(),
                        EngineEvent {
                            name: format!("collision.{phase}"),
                            source,
                            target: Some(target),
                            payload: serde_json::json!({"other":other, "phase":phase}),
                        },
                    )?;
                }
            }
        }
        Ok(())
    }
}
