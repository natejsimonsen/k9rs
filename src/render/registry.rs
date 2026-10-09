//! GVR → renderer lookup (k9s model/registry.go).

use crate::k8s::Gvr;

use super::Renderer;

/// Return the renderer for `gvr`, falling back to the generic renderer.
pub fn renderer_for(gvr: &Gvr) -> Box<dyn Renderer> {
    if let Some(r) = super::core::renderer_for(gvr) {
        return r;
    }
    if let Some(r) = super::workloads::renderer_for(gvr) {
        return r;
    }
    super::core::generic()
}
