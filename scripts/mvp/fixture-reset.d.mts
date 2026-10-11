export const FIXTURE_APPLICATION: Readonly<{ name: string; bundle_id: string; version: string }>;
export interface FixtureReset {
  resetProfile: { version: 1; task_id: string; seed: number; initial_state: Record<string, unknown> };
  modelGoal: { objective: string; allowed_scope: Record<string, unknown> };
  pins: { request_sha256: string; freeze_sha256: string; freeze_content_sha256: string };
}
export function projectFixtureReset(input: {
  requestBytes: Buffer; freezeBytes: Buffer; taskId: string; seed: number;
}): FixtureReset;
export function loadFixtureReset(input: {
  requestFile: string; freezeFile: string; taskId: string; seed: number;
}): FixtureReset;
