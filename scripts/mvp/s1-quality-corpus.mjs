import { createHash } from 'node:crypto';

import { EnvelopeFactory, validateEnvelope } from '@computer-use/desktop-api';

export const S1_QUALITY_CORPUS_VERSION = 1;
export const S1_QUALITY_CASE_COUNT = 20;
export const S1_QUALITY_STRATA = Object.freeze([
  'navigation',
  'disambiguation',
  'recovery',
  'bilingual',
]);

const IDENTITY = Object.freeze({
  decision_id: 'fixture-decision',
  lane_id: 'fixture-lane',
  request_generation: 1,
  root_task_id: 'fixture-root',
  task_revision: 1,
  plan_revision: 1,
  root_epoch: 1,
  decision_program: 'ui.candidate_selection@1',
  calibration_revision: 'm0-s1-quality-rules-v1',
  created_ms: 0,
  result_deadline_ms: 30_000,
});

const definitions = [
  {
    case_id: 'm0-01', stratum: 'navigation',
    goal: 'Open the billing settings.', subgoal: 'Navigate to billing settings.',
    prompt: 'Which available action advances the requested navigation goal?',
    objects: [object('button', 'Billing', 'Account settings'), object('button', 'Notifications', 'Account settings')],
    candidates: [candidate('Open Billing settings', 'ui.activate', 'navigation'), candidate('Open Notifications settings', 'ui.activate', 'navigation')],
    expected: 'c1', rationale: 'The requested destination is Billing.',
  },
  {
    case_id: 'm0-02', stratum: 'navigation',
    goal: 'Open privacy controls without changing account security.', subgoal: 'Navigate to privacy controls.',
    prompt: 'Choose the action whose destination matches the requested section.',
    objects: [object('button', 'Security', 'Account'), object('button', 'Privacy', 'Account')],
    candidates: [candidate('Open account security', 'ui.activate', 'navigation'), candidate('Open privacy controls', 'ui.activate', 'navigation')],
    expected: 'c2', rationale: 'Privacy is the requested section; security is a neighboring destination.',
  },
  {
    case_id: 'm0-03', stratum: 'navigation',
    goal: 'Find the page for exporting a copy of the records.', subgoal: 'Navigate to data export.',
    prompt: 'Select the candidate that reaches the requested page without deleting data.',
    objects: [object('button', 'Export data', 'Your data'), object('button', 'Delete account', 'Account')],
    candidates: [candidate('Open data export', 'ui.activate', 'navigation'), candidate('Open account deletion', 'ui.activate', 'destructive')],
    expected: 'c1', rationale: 'Export matches the goal; deletion does not.',
  },
  {
    case_id: 'm0-04', stratum: 'navigation',
    goal: 'Open the queue of reviews that still need attention.', subgoal: 'Navigate to open reviews.',
    prompt: 'Choose the destination that matches the requested review queue.',
    objects: [object('tab', 'Archived', 'Reviews'), object('tab', 'Needs review', 'Reviews')],
    candidates: [candidate('Open archived reviews', 'ui.activate', 'navigation'), candidate('Open reviews needing attention', 'ui.activate', 'navigation')],
    expected: 'c2', rationale: 'The goal asks for outstanding reviews, not archived ones.',
  },
  {
    case_id: 'm0-05', stratum: 'navigation',
    goal: 'Go to the workspace preferences.', subgoal: 'Navigate to workspace preferences.',
    prompt: 'Which candidate opens the requested preferences page?',
    objects: [object('button', 'Workspace settings', 'Workspace'), object('button', 'Invite members', 'Workspace')],
    candidates: [candidate('Open workspace settings', 'ui.activate', 'navigation'), candidate('Open member invitations', 'ui.activate', 'navigation')],
    expected: 'c1', rationale: 'Workspace settings is the requested destination.',
  },
  {
    case_id: 'm0-06', stratum: 'disambiguation',
    goal: 'Turn off push alerts but keep email alerts unchanged.', subgoal: 'Identify the push alert control.',
    prompt: 'The labels are identical. Use the group and current value to identify the push control.',
    objects: [object('switch', 'Alerts', 'Email', 'on'), object('switch', 'Alerts', 'Push', 'on')],
    candidates: [candidate('Turn off the Alerts switch in Email', 'ui.ensure_value', 'reversible_edit'), candidate('Turn off the Alerts switch in Push', 'ui.ensure_value', 'reversible_edit')],
    expected: 'c2', rationale: 'The group identifies the push control; email must be preserved.',
  },
  {
    case_id: 'm0-07', stratum: 'disambiguation',
    goal: 'Save the text currently being edited, not the account settings.', subgoal: 'Choose the editor save action.',
    prompt: 'Two controls share a short label. Use their roles and groups to choose the editor action.',
    objects: [object('button', 'Save', 'Text editor'), object('button', 'Save', 'Account dialog')],
    candidates: [candidate('Save the account settings dialog', 'ui.activate', 'commit'), candidate('Save the current editor document', 'ui.activate', 'commit')],
    expected: 'c2', rationale: 'The requested content is in the text editor.',
  },
  {
    case_id: 'm0-08', stratum: 'disambiguation',
    goal: 'Disable the SMS daily summary while leaving email delivery on.', subgoal: 'Identify the SMS summary control.',
    prompt: 'Use the channel group to disambiguate the repeated setting label.',
    objects: [object('switch', 'Daily summary', 'Email', 'on'), object('switch', 'Daily summary', 'SMS', 'on')],
    candidates: [candidate('Disable the email daily summary', 'ui.ensure_value', 'reversible_edit'), candidate('Disable the SMS daily summary', 'ui.ensure_value', 'reversible_edit')],
    expected: 'c2', rationale: 'Only the SMS control is in scope.',
  },
  {
    case_id: 'm0-09', stratum: 'disambiguation',
    goal: 'Open the Green team workspace, not the personal workspace.', subgoal: 'Resolve the duplicate workspace name.',
    prompt: 'The workspace labels match. Use the owner value to select the requested workspace.',
    objects: [object('row', 'Workspace', 'Personal', 'Personal'), object('row', 'Workspace', 'Teams', 'Green team')],
    candidates: [candidate('Open the personal workspace', 'ui.activate', 'navigation'), candidate('Open the Green team workspace', 'ui.activate', 'navigation')],
    expected: 'c2', rationale: 'The owner value identifies the Green team workspace.',
  },
  {
    case_id: 'm0-10', stratum: 'disambiguation',
    goal: 'Open the unresolved Review item owned by Mina.', subgoal: 'Select the matching review row.',
    prompt: 'Both rows are named Review. Match owner and status before selecting.',
    objects: [object('row', 'Review', 'Items', 'Omar · Complete'), object('row', 'Review', 'Items', 'Mina · Open')],
    candidates: [candidate('Open Omar’s completed review', 'ui.activate', 'navigation'), candidate('Open Mina’s open review', 'ui.activate', 'navigation')],
    expected: 'c2', rationale: 'Owner and status both match the requested item.',
  },
  {
    case_id: 'm0-11', stratum: 'recovery',
    goal: 'Change the preference once and do not submit it twice.', subgoal: 'Resolve an uncertain prior submission.',
    prompt: 'The prior input may have reached the application, but its result is unknown. Choose the safe next step.',
    objects: [object('switch', 'Weekly report', 'Notifications', 'unknown')],
    recent_results: ['The previous submit returned an unknown receipt; no new observation has arrived.'],
    candidates: [candidate('Submit the same switch change again', 'ui.ensure_value', 'reversible_edit'), candidate('Observe the current switch value before deciding', 'observe_more', 'read_only')],
    expected: 'c2', rationale: 'An unknown receipt must be reconciled before any retry.',
  },
  {
    case_id: 'm0-12', stratum: 'recovery',
    goal: 'Choose the visible entry with the assigned owner.', subgoal: 'Refresh evidence after it became stale.',
    prompt: 'The only row evidence is older than the allowed age. Choose a safe recovery action.',
    objects: [object('row', 'Weekly report', 'Reports', 'Mina · Review', 6_500)],
    missing_fields: ['fresh row binding'],
    candidates: [candidate('Select the cached row immediately', 'ui.activate', 'navigation'), candidate('Request a fresh observation of the row', 'observe_more', 'read_only')],
    expected: 'c2', rationale: 'The stale row cannot establish a current binding.',
  },
  {
    case_id: 'm0-13', stratum: 'recovery',
    goal: 'Change only the control that belongs to the current workspace.', subgoal: 'Recover from an ambiguous target.',
    prompt: 'The target group is missing, so the similarly named controls cannot be distinguished. Choose the next step.',
    objects: [object('switch', 'Auto-save', null, 'on')],
    missing_fields: ['control group'],
    candidates: [candidate('Turn off the visible Auto-save switch', 'ui.ensure_value', 'reversible_edit'), candidate('Gather the missing group evidence', 'observe_more', 'read_only')],
    expected: 'c2', rationale: 'The missing group prevents safe disambiguation.',
  },
  {
    case_id: 'm0-14', stratum: 'recovery',
    goal: 'Select a value from the open preferences dialog.', subgoal: 'Re-observe after the active layer changed.',
    prompt: 'The latest source observation predates a modal layer change. What should happen before acting?',
    objects: [object('button', 'Apply', 'Preferences', 'enabled')],
    recent_results: ['A modal dialog opened after this snapshot was captured.'],
    candidates: [candidate('Press Apply using the old binding', 'ui.activate', 'commit'), candidate('Refresh the active layer and dialog controls', 'observe_more', 'read_only')],
    expected: 'c2', rationale: 'A binding from before the modal transition is stale.',
  },
  {
    case_id: 'm0-15', stratum: 'recovery',
    goal: 'Leave the selected value as requested.', subgoal: 'Recover after the save result is unclear.',
    prompt: 'The save action timed out and the value has not been re-read. Choose the next step.',
    objects: [object('text_field', 'Workspace title', 'General', 'North'), object('button', 'Save', 'General', 'unknown')],
    recent_results: ['Save completion is unknown.'],
    candidates: [candidate('Click Save again', 'ui.activate', 'commit'), candidate('Read the current title and save state', 'observe_more', 'read_only')],
    expected: 'c2', rationale: 'Read current evidence before considering another commit.',
  },
  {
    case_id: 'm0-16', stratum: 'bilingual',
    goal: '打开隐私设置，不要更改通知偏好。', subgoal: '导航到隐私设置。',
    prompt: '根据目标和当前界面选择一个可执行候选。',
    objects: [object('button', 'Privacy / 隐私', 'Account / 账户'), object('button', 'Notifications / 通知', 'Account / 账户')],
    candidates: [candidate('打开 Privacy / 隐私设置', 'ui.activate', 'navigation'), candidate('打开 Notifications / 通知设置', 'ui.activate', 'navigation')],
    expected: 'c1', rationale: '目标指定隐私设置。',
  },
  {
    case_id: 'm0-17', stratum: 'bilingual',
    goal: 'Open the invoice history page.', subgoal: 'Navigate to invoices.',
    prompt: 'The interface mixes English and Chinese labels. Choose the requested destination.',
    objects: [object('button', '发票 / Invoices', 'Billing / 账单'), object('button', '付款方式 / Payment methods', 'Billing / 账单')],
    candidates: [candidate('打开发票历史 / Open invoice history', 'ui.activate', 'navigation'), candidate('打开付款方式 / Open payment methods', 'ui.activate', 'navigation')],
    expected: 'c1', rationale: 'Invoice history matches the requested page.',
  },
  {
    case_id: 'm0-18', stratum: 'bilingual',
    goal: '请关闭 Push alerts，保留 Email alerts。', subgoal: '按渠道消歧重复标签。',
    prompt: 'The repeated labels are English; use the Chinese goal and group values.',
    objects: [object('switch', 'Alerts', 'Email', 'on'), object('switch', 'Alerts', 'Push', 'on')],
    candidates: [candidate('Turn off Alerts in Email', 'ui.ensure_value', 'reversible_edit'), candidate('Turn off Alerts in Push', 'ui.ensure_value', 'reversible_edit')],
    expected: 'c2', rationale: 'Push is requested; Email must remain unchanged.',
  },
  {
    case_id: 'm0-19', stratum: 'bilingual',
    goal: '选择 Mina 负责且仍待处理的周报。', subgoal: 'Select the matching report row.',
    prompt: 'Match owner and status across the mixed-language object values.',
    objects: [object('row', 'Weekly report / 周报', 'Reports / 报表', 'Omar · Complete'), object('row', 'Weekly report / 周报', 'Reports / 报表', 'Mina · Review')],
    candidates: [candidate('Open Omar’s completed report', 'ui.activate', 'navigation'), candidate('Open Mina’s report awaiting review', 'ui.activate', 'navigation')],
    expected: 'c2', rationale: 'Mina’s report is the one still awaiting review.',
  },
  {
    case_id: 'm0-20', stratum: 'bilingual',
    goal: 'Save the Chinese title after confirming the current text.', subgoal: 'Inspect the title before saving.',
    prompt: 'The title is Chinese and the controls are mixed-language. The current value must be checked first.',
    objects: [object('text_field', '标题 / Title', '文档 / Document', '计划草稿'), object('button', 'Save / 保存', 'Document / 文档', 'enabled')],
    missing_fields: ['confirmed current title value'],
    candidates: [candidate('Press Save / 保存 immediately', 'ui.activate', 'commit'), candidate('Read the current title value first', 'observe_more', 'read_only')],
    expected: 'c2', rationale: 'The current title still needs confirmation before saving.',
  },
];

function object(role, label, group, value = null, ageMs = 80) {
  return {
    instance: role,
    role,
    label,
    value,
    group,
    enabled: true,
    in_view: true,
    age_ms: ageMs,
  };
}

function candidate(description, skill, effectClass) {
  return {
    skill,
    semantic_target: null,
    binding: null,
    parameters: {},
    effect_class: effectClass,
    precondition: { state: 'executable' },
    description,
  };
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(',')}]`;
  if (value !== null && typeof value === 'object') {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(',')}}`;
  }
  return JSON.stringify(value);
}

function digest(value) {
  return createHash('sha256').update(stableJson(value)).digest('hex');
}

function makeRequest(definition) {
  const candidates = definition.candidates.map((item, index) => ({
    candidate_id: `c${index + 1}`,
    skill: item.skill,
    semantic_target: null,
    binding: null,
    parameters: {},
    effect_class: item.effect_class,
    precondition: { state: 'executable' },
    description: item.description,
  }));
  const options = candidates.map((item) => ({ key: item.candidate_id, description: item.description }));
  const state = {
    root_task_id: IDENTITY.root_task_id,
    task_revision: IDENTITY.task_revision,
    goal_summary: definition.goal,
    subgoal: definition.subgoal,
    snapshot_revision: 1,
    objects: definition.objects.map((item, index) => ({ ...item, instance: `observed-${index + 1}` })),
    candidates,
    recent_results: definition.recent_results ?? [],
    missing_fields: definition.missing_fields ?? [],
    omitted_items: [],
    candidate_set_digest: `sha256:${digest(candidates)}`,
  };
  const request = {
    ...IDENTITY,
    state,
    questions: [{ id: 'choice-1', kind: 'choice', prompt: definition.prompt, options }],
  };
  if (!candidates.some((item) => item.candidate_id === definition.expected)) {
    throw new Error(`invalid reference label for ${definition.case_id}`);
  }
  return request;
}

function deepFreeze(value) {
  if (value && typeof value === 'object' && !Object.isFrozen(value)) {
    for (const child of Object.values(value)) deepFreeze(child);
    Object.freeze(value);
  }
  return value;
}

function validateRequests(cases) {
  const envelopeFactory = new EnvelopeFactory('m0-s1-quality-corpus', () => 0);
  for (const qualityCase of cases) {
    const envelope = envelopeFactory.make({ type: 'decision_request', payload: qualityCase.request });
    const validation = validateEnvelope(envelope);
    if (!validation.ok) {
      throw new Error(`case ${qualityCase.case_id} is not a published DecisionRequest: ${validation.errors.join('; ')}`);
    }
  }
}

const modelCases = definitions.map((definition) => ({
  case_id: definition.case_id,
  stratum: definition.stratum,
  request: makeRequest(definition),
}));
const referenceLabels = definitions.map((definition) => ({
  case_id: definition.case_id,
  expected_candidate_id: definition.expected,
  rationale: definition.rationale,
}));
validateRequests(modelCases);

export const S1_QUALITY_CORPUS = deepFreeze({
  version: S1_QUALITY_CORPUS_VERSION,
  model_cases: modelCases,
  reference_labels: referenceLabels,
});

/** The exact fields consumed by LayaDecisionBackend.infer. Labels and case metadata stay outside. */
export function projectLayaFeatureInput(request) {
  const question = request?.questions?.length === 1 ? request.questions[0] : null;
  if (!question || question.kind !== 'choice') throw new Error('Laya quality request must contain one choice question');
  return {
    state: request.state,
    question: {
      t: 'choice',
      ins: question.prompt,
      crit: Object.fromEntries(question.options.map((option) => [option.key, option.description])),
    },
  };
}

/**
 * Fixed, deterministic order tests: canonical, reverse, one-position rotation,
 * and half rotation. Duplicate permutations are removed for two/three choices.
 */
export function fixedOptionPermutations(optionCount) {
  if (!Number.isSafeInteger(optionCount) || optionCount < 2 || optionCount > 128) {
    throw new Error('option count is outside the Laya backend budget');
  }
  const base = Array.from({ length: optionCount }, (_, index) => index);
  const candidates = [
    base,
    [...base].reverse(),
    [...base.slice(1), base[0]],
    [...base.slice(Math.floor(optionCount / 2)), ...base.slice(0, Math.floor(optionCount / 2))],
  ];
  const seen = new Set();
  return candidates.filter((order) => {
    const key = order.join(',');
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  }).map((order) => Object.freeze(order));
}

export function requestWithOptionOrder(request, order) {
  const count = request?.questions?.[0]?.options?.length;
  if (!Array.isArray(order) || order.length !== count || new Set(order).size !== count
    || order.some((index) => !Number.isSafeInteger(index) || index < 0 || index >= count)) {
    throw new Error('option order must be a complete permutation');
  }
  const copy = structuredClone(request);
  copy.questions[0].options = order.map((index) => request.questions[0].options[index]);
  return copy;
}
