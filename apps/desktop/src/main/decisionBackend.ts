import { ScriptedTeacher, unavailable, type DecisionBackend } from '@computer-use/decision-worker';
import type { NativeAuthorization, OwnedDocumentStatus } from '@computer-use/desktop-api';

interface SessionStatus {
  native: NativeAuthorization['phase'];
  owned: OwnedDocumentStatus['phase'];
}

/** Reference answers are confined to the simulated fixture session. */
export function createSessionDecisionBackend(readSession: () => SessionStatus | null): DecisionBackend {
  const fixture = new ScriptedTeacher();
  return {
    name: 'session-decision',
    source: 's1',
    async decide(request, signal) {
      const session = readSession();
      if (signal.aborted || session?.native !== 'not_native' || session.owned !== 'not_created') {
        return { ...unavailable(request, 'local-s1-unavailable'), source: 's1' };
      }
      return fixture.decide(request);
    },
  };
}
