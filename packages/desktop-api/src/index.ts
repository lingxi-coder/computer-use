export * from './generated/envelope.js';
export type {
  ComputerFence,
  ComputerFrameRequest,
  ComputerBatchProposal,
  ComputerActionProposal,
  ComputerMouseButton,
  ComputerPoint,
} from './generated/computer.js';
export { validateComputerFence, validateComputerBatchProposal, validateComputerFrameRequest } from './computerValidate.js';
export { schemaSha256 } from './generated/schema.js';
export { frameStream, frameCapture, frameGeometry, frameWatchId, frameImageId, validSingleCapture } from './sourceProvenance.js';
export { validateEnvelope, assertEnvelope, validateOperatorCommand, type Validation } from './validate.js';
export {
  encodeFrame,
  FrameDecoder,
  FrameError,
  type FrameErrorKind,
  LENGTH_PREFIX_BYTES,
  MAX_FRAME_BYTES,
} from './frame.js';
export { EnvelopeFactory, PROTOCOL_VERSION } from './envelope.js';
export type {
  DesktopApi,
  BridgeStopStatus,
  ObservationImage,
  ImageAvailability,
  NativeWindowTarget,
  NativeAuthorization,
  OwnedDocumentStatus,
  FrozenTaskSelection,
  FrozenTaskCatalogEntry,
  FrozenTaskCatalogState,
  FrozenTaskCatalogSnapshot,
  ScopedTaskRequest,
} from './api.js';
export type { ModelProvider, ModelRoute, ModelPurpose, ModelAuthorizationRequest, OpenAiLoginRequest, ModelAuthorizationStatus, SavedModelConnection } from './api.js';

export type {
  BrowserCheckboxWitness,
  BrowserControlAuthority,
  BrowserControlBootstrap,
  BrowserControlHello,
  BrowserControlOutcome,
  BrowserControlReady,
  BrowserControlReply,
  BrowserControlSubmit,
  BrowserControlToken,
} from './generated/browser.js';
export {
  browserTargetContextHash,
  validateBrowserPageSnapshot,
  validateBrowserReadScope,
  validateBrowserSourceControl,
  validateBrowserExecutionObservation,
  validateBrowserActionIntent,
} from './browserValidate.js';
export {
  validateBrowserControlAuthority,
  validateBrowserControlBootstrap,
  validateBrowserControlHello,
  validateBrowserControlOutcome,
  validateBrowserControlReady,
  validateBrowserControlReply,
  validateBrowserControlSubmit,
} from './browserControlValidate.js';

export type { ProviderModel, ModelProviderDefinition, ProviderConnection, ProviderSettingsSnapshot, ConfigureProviderRequest, TaskModelSelection } from './api.js';
