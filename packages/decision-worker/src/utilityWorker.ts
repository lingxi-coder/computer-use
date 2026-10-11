import type { DecisionRequest } from '@computer-use/desktop-api';

import { LayaDecisionBackend } from './laya/layaBackend.js';
import { DecisionWorker, unavailable } from './worker.js';
import {
  decodeUtilityFrame,
  encodeUtilityFrame,
  toUtilityTransferable,
  UTILITY_PROTOCOL_VERSION,
  type UtilityFailureCode,
  type UtilityMainMessage,
  type UtilityWorkerMessage,
} from './utilityProtocol.js';

interface UtilityParentPort {
  on(event: 'message', listener: (event: { data: unknown }) => void): unknown;
  postMessage(message: ArrayBuffer): void;
}

type WorkerState = 'awaiting_init' | 'initializing' | 'ready' | 'unavailable' | 'closing';

interface ActiveRequest {
  request_id: number;
  epoch: number;
  request: DecisionRequest;
}

export async function runUtilityWorker(port: UtilityParentPort): Promise<void> {
  let state: WorkerState = 'awaiting_init';
  const currentState = (): WorkerState => state;
  let epoch = 1;
  let backend: LayaDecisionBackend | null = null;
  let decisions: DecisionWorker | null = null;
  let active: ActiveRequest | null = null;
  let shutdownPromise: Promise<void> | null = null;

  const send = (message: UtilityWorkerMessage): boolean => {
    try {
      port.postMessage(toUtilityTransferable(encodeUtilityFrame(message)));
      return true;
    } catch {
      return false;
    }
  };

  const fixedUnavailable = (
    request: DecisionRequest,
    requestId: number,
    requestEpoch: number,
    code: UtilityFailureCode,
  ): UtilityWorkerMessage => ({
    version: UTILITY_PROTOCOL_VERSION,
    type: 'unavailable',
    request_id: requestId,
    epoch: requestEpoch,
    code,
    proposal: unavailable(request, backend?.name ?? 'local-s1-unavailable', 's1'),
  });

  const shutdown = (): Promise<void> => {
    if (shutdownPromise) return shutdownPromise;
    state = 'closing';
    decisions?.invalidate();
    shutdownPromise = (async () => {
      const closing = backend?.close().catch(() => undefined);
      if (!closing) return;
      await Promise.race([
        closing,
        new Promise<void>((resolve) => {
          const timeout = setTimeout(resolve, 1_000);
          timeout.unref?.();
        }),
      ]);
    })();
    return shutdownPromise;
  };

  const sendProtocolFailure = (): void => {
    state = 'closing';
    send({ version: UTILITY_PROTOCOL_VERSION, type: 'unavailable', code: 'invalid_message' });
    void shutdown().finally(() => process.exit(1));
  };

  const initialize = async (message: Extract<UtilityMainMessage, { type: 'init' }>): Promise<void> => {
    if (state !== 'awaiting_init') {
      sendProtocolFailure();
      return;
    }
    state = 'initializing';
    try {
      backend = await LayaDecisionBackend.create({
        bundleManifestPath: message.bundleManifestPath,
        numThreads: message.numThreads,
        minimumConfidenceMilli: message.minimumConfidenceMilli,
      });
      if (currentState() === 'closing') {
        await backend.close().catch(() => undefined);
        backend = null;
        return;
      }
      decisions = new DecisionWorker(backend);
      state = 'ready';
      if (!send({
        version: UTILITY_PROTOCOL_VERSION,
        type: 'ready',
        model_revision: backend.modelRevision,
      })) {
        await shutdown();
      }
    } catch {
      state = 'unavailable';
      send({ version: UTILITY_PROTOCOL_VERSION, type: 'unavailable', code: 'backend_unavailable' });
    }
  };

  const handleRequest = (message: Extract<UtilityMainMessage, { type: 'request' }>): void => {
    if (message.epoch !== epoch) {
      send(fixedUnavailable(message.request, message.request_id, message.epoch, 'invalid_request'));
      return;
    }
    if (state === 'unavailable' || !decisions || !backend) {
      send(fixedUnavailable(message.request, message.request_id, message.epoch, 'backend_unavailable'));
      return;
    }
    if (state !== 'ready' || active) {
      send(fixedUnavailable(message.request, message.request_id, message.epoch, 'worker_busy'));
      return;
    }

    const current: ActiveRequest = {
      request_id: message.request_id,
      epoch: message.epoch,
      request: message.request,
    };
    active = current;
    void decisions.submit(message.request).then((delivery) => {
      if (state === 'closing') return;
      if (active !== current) return;
      if (delivery.kind === 'proposal') {
        if (
          delivery.proposal.source === 's1'
          && delivery.proposal.decision_id === current.request.decision_id
          && delivery.proposal.request_generation === current.request.request_generation
          && delivery.proposal.candidate_set_digest === current.request.state.candidate_set_digest
          && send({
            version: UTILITY_PROTOCOL_VERSION,
            type: 'proposal',
            request_id: current.request_id,
            epoch: current.epoch,
            proposal: delivery.proposal,
          })
        ) {
          return;
        }
        send(fixedUnavailable(current.request, current.request_id, current.epoch, 'invalid_proposal'));
        return;
      }
      send(fixedUnavailable(current.request, current.request_id, current.epoch, 'worker_busy'));
    }).catch(() => {
      if (state !== 'closing' && active === current) {
        send(fixedUnavailable(current.request, current.request_id, current.epoch, 'backend_unavailable'));
      }
    }).finally(() => {
      if (active === current) active = null;
    });
  };

  const handleMessage = (message: UtilityMainMessage): void => {
    if (message.type === 'init') {
      void initialize(message);
      return;
    }
    if (message.type === 'close') {
      void shutdown().finally(() => process.exit(0));
      return;
    }
    if (state === 'awaiting_init' || state === 'initializing' || state === 'closing') {
      sendProtocolFailure();
      return;
    }
    switch (message.type) {
      case 'request':
        handleRequest(message);
        break;
      case 'cancel':
        if (active?.request_id === message.request_id && decisions) {
          decisions.cancel({
            decision_id: message.decision_id,
            request_generation: message.request_generation,
          });
        }
        break;
      case 'invalidate':
        if (message.epoch !== epoch + 1) {
          sendProtocolFailure();
          return;
        }
        epoch = message.epoch;
        decisions?.invalidate();
        break;
      default:
        sendProtocolFailure();
    }
  };

  port.on('message', (event) => {
    try {
      const message = decodeUtilityFrame(event.data);
      if (message.type !== 'init' && message.type !== 'request' && message.type !== 'cancel'
        && message.type !== 'invalidate' && message.type !== 'close') {
        sendProtocolFailure();
        return;
      }
      handleMessage(message);
    } catch {
      sendProtocolFailure();
    }
  });

  process.once('SIGTERM', () => {
    void shutdown().finally(() => process.exit(0));
  });
}

const parentPort = (process as NodeJS.Process & { parentPort?: UtilityParentPort }).parentPort;
if (parentPort) void runUtilityWorker(parentPort);
