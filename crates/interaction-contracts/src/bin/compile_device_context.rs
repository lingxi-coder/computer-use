//! Read-only diagnostic access to the production binding-context compiler.
//! This emits facts from the supplied Source snapshot, never a permit.

use std::io::{self, Read, Write};

use interaction_contracts::desktop::binding::{
    BindingProofScope, derive_background_ax_binding_evidence, derive_binding_evidence,
    derive_window_root_binding_evidence,
};
use interaction_contracts::desktop::ids::ObjectRef;
use interaction_contracts::desktop::source::StructureSnapshot;
use serde::Deserialize;

const MAX_INPUT_BYTES: usize = 768 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    snapshot: StructureSnapshot,
    target: ObjectRef,
    scope: BindingProofScope,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = Vec::new();
    io::stdin()
        .lock()
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut input)?;
    if input.len() > MAX_INPUT_BYTES {
        return Err("device-context diagnostic input exceeds its byte limit".into());
    }
    let request: Request = serde_json::from_slice(&input)?;
    let target = request
        .snapshot
        .objects
        .iter()
        .find(|object| object.instance_ref == request.target)
        .ok_or("device-context diagnostic target is absent from the supplied snapshot")?;
    let evidence = match &request.scope {
        BindingProofScope::WindowRoot { table_scope } => {
            derive_window_root_binding_evidence(&request.snapshot, target, table_scope)?
        }
        BindingProofScope::BackgroundAx {
            target_context_hash: requested_hash,
        } => {
            // The Source snapshot is the authority for this hash. Derive the
            // BG witness from its exact target context, then require the input
            // hash to be an assertion of that value rather than an override.
            let evidence = derive_background_ax_binding_evidence(&request.snapshot, target)?;
            let BindingProofScope::BackgroundAx {
                target_context_hash: snapshot_hash,
            } = &evidence.continuity.scope
            else {
                return Err("background AX compiler returned an unexpected proof scope".into());
            };
            if requested_hash != snapshot_hash {
                return Err(
                    "requested background AX hash differs from the supplied Source target context"
                        .into(),
                );
            }
            evidence
        }
        scope => derive_binding_evidence(&request.snapshot, target, scope.clone())?,
    };
    let mut output = io::stdout().lock();
    serde_json::to_writer(
        &mut output,
        &serde_json::json!({
            "continuity": evidence.continuity,
            "device_context": evidence.device_context,
        }),
    )?;
    writeln!(output)?;
    Ok(())
}
