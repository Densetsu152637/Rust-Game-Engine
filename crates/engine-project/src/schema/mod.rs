use crate::{
    Error, Result,
    input::{BindingMap, validate_bindings},
};
use bevy_ecs::prelude::Component;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component as PathComponent, Path, PathBuf},
};

mod components;
mod docs;
mod files;
mod light_docs;
mod model;
mod post_docs;
mod project;
mod validate;

pub use components::*;
pub use docs::*;
pub(crate) use files::read_bounded;
pub use files::{confined_file, load_scene};
use model::Manifest;
pub use model::*;
use validate::identifier;
