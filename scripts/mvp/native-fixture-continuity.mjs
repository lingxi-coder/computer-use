import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const compiler = fileURLToPath(new URL('../../target/debug/compile_device_context', import.meta.url));

export function buildNativeFixtureContextCompiler() {
  execFileSync('cargo', ['build', '--offline', '--locked', '-p', 'interaction-contracts',
    '--bin', 'compile_device_context'], { cwd: root, stdio: 'inherit' });
}

/**
 * Direct native-fixture diagnostics use the production Rust closure compiler.
 * The header is explicitly diagnostic, with no Host clock or permission claim.
 * Even covered Source objects receive a stricter read-scoped diagnostic proof.
 */
export function nativeFixtureBindingEvidence(structure, object, sourceSequence) {
  if (!Number.isSafeInteger(sourceSequence) || sourceSequence <= 0
    || structure?.complete !== true || !Array.isArray(structure.objects)
    || structure.objects.length === 0 || structure.objects.length > 512) {
    throw new Error('native fixture continuity requires one complete bounded Source read');
  }
  if (Object.hasOwn(structure, 'active_layer')
    || !Object.hasOwn(structure, 'global_focus')
    || !Object.hasOwn(structure, 'target_context')
    || structure.target_context === undefined) {
    throw new Error('native fixture Source requires global_focus and target_context and rejects legacy active_layer');
  }
  const refs = new Set();
  let covered = 0;
  let scoped = 0;
  for (const object of structure.objects) {
    if (typeof object.instance_ref !== 'string' || !object.instance_ref
      || refs.has(object.instance_ref)) {
      throw new Error('native fixture Source object references are missing or ambiguous');
    }
    refs.add(object.instance_ref);
    if (object.identity_continuity === 'notification_covered') covered += 1;
    else if (object.identity_continuity === 'read_scoped') scoped += 1;
    else throw new Error('native fixture Source object continuity is unknown');
  }
  const coverage = structure.observer_coverage;
  if (!coverage || !['available', 'degraded', 'unavailable'].includes(coverage.state)
    || coverage.covered_objects !== covered || coverage.read_scoped_objects !== scoped) {
    throw new Error('native fixture Source coverage does not match its objects');
  }
  if (!Number.isSafeInteger(coverage.failed_registrations)
    || coverage.failed_registrations < 0 || coverage.failed_registrations > 4096
    || !Array.isArray(coverage.failures)
    || coverage.failures.length !== Math.min(coverage.failed_registrations, 16)) {
    throw new Error('native fixture Source registration failures are missing or unbounded');
  }
  if ((coverage.state === 'available' && (scoped !== 0 || coverage.failed_registrations !== 0))
    || (coverage.state === 'unavailable' && covered !== 0)
    || (coverage.state === 'degraded' && scoped === 0 && coverage.failed_registrations === 0)) {
    throw new Error('native fixture Source coverage state contradicts its evidence');
  }
  if (!object || !structure.objects.some(candidate => candidate.instance_ref === object.instance_ref)) {
    throw new Error('native fixture target is absent from the current Source read');
  }
  const contexts = [...structure.menu_anchors ?? [], structure.menu_context,
    structure.menu_context?.opener, structure.focus_context, structure.window_context].filter(Boolean);
  const stamps = contexts.filter(context => Object.hasOwn(context, 'verified_source_sequence'));
  if (stamps.some(context => context.verified_source_sequence !== sourceSequence)) {
    throw new Error('native fixture Source context sequence does not match the supplied read');
  }
  const observed = new Set(stamps.map(context => context.verified_observed_ms));
  const epochs = new Set(contexts.filter(context => Object.hasOwn(context, 'session_epoch'))
    .map(context => context.session_epoch));
  if (observed.size > 1 || epochs.size > 1) {
    throw new Error('native fixture Source context provenance is inconsistent');
  }
  const snapshot = { ...structuredClone(structure), snapshot_id: `diagnostic-source-${sourceSequence}`,
    session_epoch: epochs.values().next().value ?? 1,
    surface: { id: 'diagnostic', epoch: 1 }, clock_domain: 'diagnostic/unmapped',
    source_sequence: sourceSequence, observed_ms: observed.values().next().value ?? 0,
    identity_boundary: structure.identity_boundary ?? null,
    owned_document_evidence: null };
  const encoded = JSON.stringify({ snapshot, target: object.instance_ref,
    scope: { kind: 'whole_structure' } });
  if (Buffer.byteLength(encoded) > 768 * 1024) {
    throw new Error('native fixture Source exceeds the diagnostic compiler input limit');
  }
  const evidence = JSON.parse(execFileSync(compiler, [], { input: encoded,
    encoding: 'utf8', timeout: 3000, maxBuffer: 16 * 1024 }));
  if (evidence.device_context?.scope !== 'whole_structure_v1'
    || evidence.continuity?.scope?.kind !== 'whole_structure'
    || evidence.continuity.source_sequence !== sourceSequence) {
    throw new Error('native fixture Source has no supported current Device context');
  }
  return { continuity: { ...evidence.continuity, kind: 'read_scoped' },
    device_context: evidence.device_context };
}

/** Native wire carries one current witness, rather than repeating it in proofs. */
export function nativeFixtureAuthorityContext(proof) {
  const { device_context, ...nativeProof } = proof;
  if (device_context?.scope !== 'whole_structure_v1') {
    throw new Error('native fixture authority requires a compiled current witness');
  }
  return { original_proof: structuredClone(nativeProof), current_proof: structuredClone(nativeProof),
    context_witness: structuredClone(device_context) };
}
