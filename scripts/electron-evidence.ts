export interface NativeFrameEvidence {
  [key: string]: unknown;
  ok?: boolean;
  requires_native_frame?: boolean;
  checks?: string[];
  binary_image?: { decoded?: boolean } | null;
  observation?: { ownsTarget?: boolean; healthyFrame?: boolean; sources?: { source: string; state: string }[] } | null;
  healthy_frame_screenshot_created?: boolean;
  tested_at_utc?: string;
}

export interface RuntimeEvidence {
  limitations?: Record<string, unknown>;
  run_history?: Record<string, NativeFrameEvidence[]>;
  limitation_history?: Record<string, unknown[]>;
}

export function reconcileNativeFrameEvidence(proof: RuntimeEvidence, runName: string, previousRun: NativeFrameEvidence | undefined, run: NativeFrameEvidence, exitCode: number | null): void {
  if (run.requires_native_frame && previousRun) {
    proof.run_history ??= {};
    proof.run_history[runName] = [...(proof.run_history[runName] ?? []), previousRun].slice(-8);
  }
  const verified = run.requires_native_frame === true && run.ok === true && exitCode === 0
    && run.observation?.ownsTarget === true && run.observation.healthyFrame === true
    && run.observation.sources?.some(({ source, state }) => source === 'screenshot' && state === 'available')
    && run.healthy_frame_screenshot_created === true
    && ['healthy owned-fixture screenshot reaches the Operator', 'native frame dimensions are rendered', 'native image and confirmation ages are rendered', 'native PNG pixels are decoded through the binary channel']
      .every((check) => run.checks?.includes(check));
  if (!run.requires_native_frame) return;
  const previousLimitation = proof.limitations?.native_frame_ui;
  if (previousLimitation && typeof previousLimitation === 'object') {
    proof.limitation_history ??= {};
    proof.limitation_history.native_frame_ui = [...(proof.limitation_history.native_frame_ui ?? []), previousLimitation].slice(-8);
  }
  proof.limitations ??= {};
  proof.limitations.native_frame_ui = {
    verified: !!verified, reason: verified ? null : 'latest_native_frame_run_failed', screen_locked: null,
    evidence_run: runName, tested_at_utc: run.tested_at_utc,
    detail: verified ? 'Healthy owned-fixture native PNG preview, dimensions and both ages passed. Screen lock state was not queried.' : 'Latest native frame UI run did not pass. Earlier evidence remains in history; screen lock state was not queried.',
  };
}
