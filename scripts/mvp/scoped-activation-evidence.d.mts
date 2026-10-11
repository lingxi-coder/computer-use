export interface ScopedActivationVerification {
  ok: boolean;
  reason?: string;
  intent_id?: string;
  attempt_id?: string;
  request_id?: string;
  source_snapshot_id?: string;
  source_sequence?: number;
  committed_ms?: number;
  returned_ms?: number;
  observed_ms?: number;
  native_primitive?: 'activate_window';
  native_call_count?: 'unknown';
  root_goal_complete?: false;
  business_gate_eligible?: false;
}

export function verifyScopedActivation(input: {
  receipt: unknown;
  rootTask: unknown;
  attempt: unknown;
  effect: unknown;
  source: unknown;
  sourceProof: unknown;
  target: unknown;
  sessionEpoch: number;
}): ScopedActivationVerification;
