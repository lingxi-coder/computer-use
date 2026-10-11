import type { RootView } from '@computer-use/desktop-api';
import type { OwnedDocumentSourceBootstrap } from './ownedDocumentBootstrap.js';

/** Host/Main private data. None of these types are part of desktop-api. */
export interface OwnedRevisitChallenge {
  version: 1;
  transition_id: string;
  root_task_id: string;
  task_revision: number;
  root_epoch: number;
  old_registration_id: string;
  save_attempt_id: string;
  save_intent_id: string;
  save_span: { from_ms: number; to_ms: number };
  expected_sha256: string;
  expected_byte_length: number;
  old_device_session_epoch: number;
  expected_device_session_epoch: number;
  old_source_namespace: string;
  clock_domain: string;
  deadline_ms: number;
}

export function validateOwnedRevisitChallenge(value: unknown, registration: OwnedDocumentSourceBootstrap,
  root: RootView | null, sessionEpoch: number, nowMs: number): OwnedRevisitChallenge {
  const invalid = () => new Error('private owned revisit challenge invalid');
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw invalid();
  const record = value as Record<string, unknown>;
  if (Object.keys(record).sort().join(',') !== [
    'version', 'transition_id', 'root_task_id', 'task_revision', 'root_epoch', 'old_registration_id',
    'save_attempt_id', 'save_intent_id', 'save_span', 'expected_sha256', 'expected_byte_length',
    'old_device_session_epoch', 'expected_device_session_epoch', 'old_source_namespace', 'clock_domain', 'deadline_ms',
  ].sort().join(',')) throw invalid();
  const challenge = record as unknown as OwnedRevisitChallenge;
  const text = (v: unknown, max: number): v is string => typeof v === 'string' && v.length > 0
    && v.length <= max && !/[\u0000-\u001f\u007f]/.test(v);
  const integer = (v: unknown, min = 1): v is number => Number.isSafeInteger(v) && (v as number) >= min;
  if (challenge.version !== 1 || !root || root.root_task_id !== challenge.root_task_id
    || root.task_revision !== challenge.task_revision || root.root_epoch !== challenge.root_epoch
    || !['running', 'waiting_evidence'].includes(root.state)
    || !text(challenge.root_task_id, 256) || !integer(challenge.task_revision) || !integer(challenge.root_epoch)
    || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(challenge.transition_id)
    || challenge.old_registration_id !== registration.registration_id
    || !text(challenge.save_attempt_id, 256) || !text(challenge.save_intent_id, 256)
    || !/^[0-9a-f]{64}$/.test(challenge.expected_sha256)
    || !integer(challenge.expected_byte_length, 0) || challenge.expected_byte_length > 2048
    || !integer(challenge.old_device_session_epoch) || challenge.old_device_session_epoch !== sessionEpoch
    || !integer(challenge.expected_device_session_epoch) || challenge.expected_device_session_epoch !== sessionEpoch + 1
    || !text(challenge.old_source_namespace, 256) || !text(challenge.clock_domain, 128)
    || !integer(challenge.deadline_ms) || challenge.deadline_ms <= nowMs || challenge.deadline_ms > nowMs + 30_000
    || !challenge.save_span || typeof challenge.save_span !== 'object' || Array.isArray(challenge.save_span)
    || Object.keys(challenge.save_span).sort().join(',') !== 'from_ms,to_ms'
    || !integer(challenge.save_span.from_ms) || !integer(challenge.save_span.to_ms)
    || challenge.save_span.from_ms > challenge.save_span.to_ms || challenge.save_span.to_ms > nowMs) throw invalid();
  return structuredClone(challenge);
}
