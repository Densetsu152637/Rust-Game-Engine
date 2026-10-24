//! 2D to 3D proxy synthesizer (section 5.2 b).

use crate::world::AcousticWorldBuilder;
use crate::{Footprint, Proxy2DSettings, ProxyMapping, Result, unimplemented};

impl AcousticWorldBuilder {
    pub fn add_proxy_2d(
        &mut self,
        _footprints: &[Footprint],
        _settings: &Proxy2DSettings,
    ) -> Result<ProxyMapping> {
        Err(unimplemented("AcousticWorldBuilder::add_proxy_2d"))
    }
}
