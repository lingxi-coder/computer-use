import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { closeSync, constants, fstatSync, fsyncSync, lstatSync, openSync, readSync, realpathSync, statSync, unlinkSync, writeSync } from 'node:fs';
import { isAbsolute, relative, resolve, sep } from 'node:path';
import { fixtureTaskIds, textEditTaskIds } from './business-results.mjs';
import { buildFrozenGoalCatalog } from './frozen-goals.mjs';

export const TASK_PROFILE_VERSION = 1;
export const FIXTURE_SEEDS = Object.freeze([1, 2, 3, 4, 5]);
export const TEXTEDIT_SEED = 1;
export const FIXTURE_TASK_COUNT = 20;
export const FIXTURE_RUN_COUNT = 100;
export const TEXTEDIT_RUN_COUNT = 20;
export const MAX_ENVIRONMENT_JSON_BYTES = 1024 * 1024;
export const MAX_ASSET_BYTES = 16 * 1024 * 1024 * 1024;
export const MAX_TOTAL_ASSET_READ_BYTES = 64 * 1024 * 1024 * 1024;

const LAYOUTS = Object.freeze(['compact', 'standard', 'wide', 'two_column']);
const THEME_NAMES = Object.freeze(['light', 'dark']);
const LANGUAGE_NAMES = Object.freeze(['en-US', 'zh-CN']);
const ARTIFACT_NAMES = Object.freeze(['app', 'textedit_app', 'model', 'graph', 'preprocess', 'program', 'knowledge']);
const HEX_SHA256 = /^[a-f0-9]{64}$/;
const CONTROL_CHARS = /[\u0000-\u001f\u007f-\u009f]/;
const INVALID_OBJECTIVE_CONTROLS = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/;
const REQUEST_FORBIDDEN_KEY = /^(?:evaluator(?:_truth)?|truth|oracle|expected_(?:result|final_state|value)|navigation|steps|workflow|selectors?|coordinates?)$/i;

const copy = value => JSON.parse(JSON.stringify(value));

function deepFreeze(value) {
  if (Buffer.isBuffer(value)) return value;
  if (value && typeof value === 'object' && !Object.isFrozen(value)) {
    for (const child of Object.values(value)) deepFreeze(child);
    Object.freeze(value);
  }
  return value;
}

function nonempty(value, label, maxBytes = 512) {
  assert.equal(typeof value, 'string', `${label} is required`);
  assert.ok(value.trim().length > 0 && Buffer.byteLength(value, 'utf8') <= maxBytes && !CONTROL_CHARS.test(value), `${label} is invalid`);
  return value.trim();
}

function validateObjective(value) {
  assert.equal(typeof value, 'string', 'user objective is required');
  assert.ok(value.trim().length > 0 && Buffer.byteLength(value, 'utf8') <= 32768
    && !INVALID_OBJECTIVE_CONTROLS.test(value), 'user objective is invalid');
  return value;
}

function finitePositive(value, label, max = Number.MAX_SAFE_INTEGER) {
  assert.ok(typeof value === 'number' && Number.isFinite(value) && value > 0 && value <= max, `${label} is invalid`);
  return value;
}

function positiveInteger(value, label, max = Number.MAX_SAFE_INTEGER) {
  assert.ok(Number.isSafeInteger(value) && value > 0 && value <= max, `${label} is invalid`);
  return value;
}

function exactKeys(value, keys, label) {
  assert.ok(value && typeof value === 'object' && !Array.isArray(value), `${label} is required`);
  assert.deepEqual(Object.keys(value).sort(), [...keys].sort(), `${label} has an unexpected shape`);
}

function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (value !== null && typeof value === 'object') {
    return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}`;
  }
  const text = JSON.stringify(value);
  assert.notEqual(text, undefined, 'unsupported JSON value');
  return text;
}

export function stableJson(value) {
  return `${canonical(value)}\n`;
}

export function sha256(value) {
  return createHash('sha256').update(value).digest('hex');
}

export function canonicalSha256(value) {
  return sha256(stableJson(value));
}

export function assignTaskVariants(taskIndex, seed) {
  assert.ok(Number.isInteger(taskIndex) && taskIndex >= 0 && taskIndex < FIXTURE_TASK_COUNT, 'fixture task index is out of range');
  assert.ok(Number.isInteger(seed) && FIXTURE_SEEDS.includes(seed), 'fixture seed is outside the frozen assignment');
  const seedIndex = seed - 1;
  return deepFreeze({
    language: LANGUAGE_NAMES[(taskIndex + seedIndex) % LANGUAGE_NAMES.length],
    theme: THEME_NAMES[(taskIndex + Math.floor(seedIndex / 2)) % THEME_NAMES.length],
    layout: LAYOUTS[(taskIndex + seedIndex) % LAYOUTS.length],
  });
}

const localized = (language, en, zh) => language === 'zh-CN' ? zh : en;
const seedCode = seed => String(seed).padStart(2, '0');
const requestedText = (language, kind, seed) => localized(language,
  `${kind} value ${seedCode(seed)}`, `${kind}目标值${seedCode(seed)}`);

const fixtureSpecs = [
  {
    id: 'SET-01',
    scope: language => ({ control: localized(language, 'Email alerts', '邮件提醒'), group: localized(language, 'Notifications', '通知') }),
    request: (language) => ({
      objective: localized(language, 'Ensure Email alerts are off. They are already off; leave the setting unchanged.', '确保“邮件提醒”已关闭。它当前已经关闭；保持原样，不要改动。'),
      initial: language => ({ controls: [{ label: localized(language, 'Email alerts', '邮件提醒'), group: localized(language, 'Notifications', '通知'), value: false }] }),
      truth: { required_final_state: { email_alerts: false }, required_change_count: 0 },
    }),
  },
  {
    id: 'SET-02',
    scope: language => ({ control: localized(language, 'Weekly digest', '每周摘要'), group: localized(language, 'Notifications', '通知') }),
    request: language => ({
      objective: localized(language, 'Turn off Weekly digest.', '关闭“每周摘要”。'),
      initial: { controls: [{ label: localized(language, 'Weekly digest', '每周摘要'), value: true }] },
      truth: { required_final_state: { weekly_digest: false } },
    }),
  },
  {
    id: 'SET-03',
    scope: language => ({ control: localized(language, 'Include archived items', '包含已归档项目'), group: localized(language, 'Search', '搜索') }),
    request: (language, seed) => ({
      objective: localized(language, `Turn on Include archived items and explicitly save the change.`, '开启“包含已归档项目”，并明确保存这项更改。'),
      initial: { controls: [{ label: localized(language, 'Include archived items', '包含已归档项目'), value: false }], persistence: 'manual_save_required', seed },
      truth: { required_final_state: { include_archived_items: true }, persistence: 'explicit_save_receipt' },
    }),
  },
  {
    id: 'SET-04',
    scope: language => ({ control: localized(language, 'Quiet hours', '免打扰时段'), group: localized(language, 'Notifications', '通知') }),
    request: (language, seed) => ({
      objective: localized(language, 'Enable Quiet hours and wait until the app reports that the change has been saved.', '开启“免打扰时段”，并等待应用确认更改已保存。'),
      initial: { controls: [{ label: localized(language, 'Quiet hours', '免打扰时段'), value: false }], persistence: 'automatic', seed },
      truth: { required_final_state: { quiet_hours: true }, persistence: 'autosave_completed_before_finish' },
    }),
  },
  {
    id: 'SET-05',
    scope: language => ({ control: localized(language, 'Compact rows', '紧凑行'), group: localized(language, 'Display', '显示') }),
    request: language => ({
      objective: localized(language, 'Turn on Compact rows and verify it remains on after leaving and returning to this setting.', '开启“紧凑行”，离开并返回此设置后确认它仍然开启。'),
      initial: { controls: [{ label: localized(language, 'Compact rows', '紧凑行'), value: false }], persistence: 'automatic' },
      truth: { required_final_state: { compact_rows: true }, required_fresh_readback: true },
    }),
  },
  {
    id: 'SET-06',
    scope: language => ({ control: localized(language, 'Show alerts', '显示提醒'), group: localized(language, 'System notifications', '系统通知'), disambiguator: localized(language, 'System notifications', '系统通知') }),
    request: language => ({
      objective: localized(language, 'Turn off Show alerts in System notifications. Leave the same-named control in Workspace notifications unchanged.', '关闭“系统通知”分组中的“显示提醒”。保持“工作区通知”分组中同名控件不变。'),
      initial: { controls: [
        { label: localized(language, 'Show alerts', '显示提醒'), group: localized(language, 'System notifications', '系统通知'), value: true },
        { label: localized(language, 'Show alerts', '显示提醒'), group: localized(language, 'Workspace notifications', '工作区通知'), value: false },
      ] },
      truth: { required_final_state: { system_show_alerts: false, workspace_show_alerts: false }, unchanged_controls: ['workspace_show_alerts'] },
    }),
  },
  {
    id: 'FORM-01',
    scope: language => ({ control: localized(language, 'Display name', '显示名称'), form: localized(language, 'Profile', '个人资料') }),
    request: (language, seed) => {
      const value = requestedText(language, localized(language, 'profile', '资料'), seed);
      return { objective: localized(language, `Set the single-line Display name to “${value}”.`, `将单行“显示名称”设为“${value}”。`),
        initial: { controls: [{ label: localized(language, 'Display name', '显示名称'), value: localized(language, 'Old name', '旧名称'), input_kind: 'single_line' }] },
        truth: { required_final_state: { display_name: value } } };
    },
  },
  {
    id: 'FORM-02',
    scope: language => ({ control: localized(language, 'Notes', '备注'), form: localized(language, 'Profile', '个人资料') }),
    request: (language, seed) => {
      const value = localized(language, `First line ${seedCode(seed)}\nSecond line ${seedCode(seed)}`, `第一行${seedCode(seed)}\n第二行${seedCode(seed)}`);
      return { objective: localized(language, `Set the multiline Notes field to exactly:\n${value}`, `将多行“备注”字段精确设为：\n${value}`),
        initial: { controls: [{ label: localized(language, 'Notes', '备注'), value: '', input_kind: 'multiline' }] },
        truth: { required_final_state: { notes: value }, line_count: 2 } };
    },
  },
  {
    id: 'FORM-03',
    scope: language => ({ control: localized(language, 'Reminder cadence', '提醒频率'), form: localized(language, 'Preferences', '偏好设置') }),
    request: (language, seed) => {
      const value = localized(language, seed % 2 ? 'Weekly' : 'Monthly', seed % 2 ? '每周' : '每月');
      return { objective: localized(language, `Set Reminder cadence to ${value}.`, `将“提醒频率”设为${value}。`),
        initial: { controls: [{ label: localized(language, 'Reminder cadence', '提醒频率'), value: localized(language, 'Daily', '每天'), input_kind: 'dropdown' }] },
        truth: { required_final_state: { reminder_cadence: value } } };
    },
  },
  {
    id: 'FORM-04',
    scope: language => ({ control: localized(language, 'Summary grouping', '摘要分组'), form: localized(language, 'Preferences', '偏好设置') }),
    request: (language, seed) => {
      const value = localized(language, seed % 2 ? 'By week' : 'By month', seed % 2 ? '按周' : '按月');
      return { objective: localized(language, `Set Summary grouping to ${value}.`, `将“摘要分组”设为${value}。`),
        initial: { controls: [{ label: localized(language, 'Summary grouping', '摘要分组'), value: localized(language, 'By day', '按天'), input_kind: 'radio_group' }] },
        truth: { required_final_state: { summary_grouping: value } } };
    },
  },
  {
    id: 'FORM-05',
    scope: language => ({ control: localized(language, 'Notify on change', '更改时通知'), form: localized(language, 'Preferences', '偏好设置') }),
    request: (language, seed) => {
      const target = seed % 2 === 1;
      return { objective: localized(language, `${target ? 'Select' : 'Clear'} Notify on change.`, `${target ? '勾选' : '取消勾选'}“更改时通知”。`),
        initial: { controls: [{ label: localized(language, 'Notify on change', '更改时通知'), value: !target, input_kind: 'checkbox' }] },
        truth: { required_final_state: { notify_on_change: target } } };
    },
  },
  {
    id: 'FORM-06',
    scope: language => ({ control: localized(language, 'Region code', '地区代码'), form: localized(language, 'Account', '账户') }),
    request: (language, seed) => {
      const value = `CA-${String(2000 + seed).slice(-4)}`;
      return { objective: localized(language, `Correct Region code to ${value} so the form passes validation.`, `将“地区代码”更正为${value}，使表单通过验证。`),
        initial: { controls: [{ label: localized(language, 'Region code', '地区代码'), value: '??', input_kind: 'single_line' }], validation_state: 'invalid' },
        truth: { required_final_state: { region_code: value }, validation_state: 'valid' } };
    },
  },
  {
    id: 'FORM-07',
    scope: language => ({ control: localized(language, 'Alert threshold', '提醒阈值'), form: localized(language, 'Advanced preferences', '高级偏好设置') }),
    request: (language, seed) => {
      const value = 20 + seed;
      return { objective: localized(language, `Set Alert threshold to ${value}.`, `将“提醒阈值”设为${value}。`),
        initial: { controls: [{ label: localized(language, 'Alert threshold', '提醒阈值'), value: 10, input_kind: 'number', initially_in_viewport: false }] },
        truth: { required_final_state: { alert_threshold: value } } };
    },
  },
  {
    id: 'LIST-01',
    scope: language => ({ list: localized(language, 'Saved views', '已保存视图'), target: localized(language, 'Quarterly review', '季度回顾') }),
    request: (language, seed) => ({
      objective: localized(language, `Open the Saved views entry named “Quarterly review ${seedCode(seed)}”.`, `打开“已保存视图”中名为“季度回顾 ${seedCode(seed)}”的条目。`),
      initial: { list: { label_lookup: true, rows: [{ label: `${localized(language, 'Quarterly review', '季度回顾')} ${seedCode(seed)}`, selected: false }] } },
      truth: { required_selected_row: `quarterly_review_${seedCode(seed)}` },
    }),
  },
  {
    id: 'LIST-02',
    scope: language => ({ list: localized(language, 'Projects', '项目'), target: localized(language, 'Weekly report', '每周报告'), context: localized(language, 'Owner and status', '负责人和状态') }),
    request: (language, seed) => ({
      objective: localized(language, `Open the Weekly report row owned by ${seed % 2 ? 'Mina' : 'Omar'} with status ${seed % 2 ? 'Review' : 'Draft'}. Leave the other same-named row untouched.`, `打开负责人为${seed % 2 ? 'Mina' : 'Omar'}且状态为“${seed % 2 ? '审核' : '草稿'}”的“每周报告”行。不要改动另一条同名行。`),
      initial: { list: { same_named_rows: [
        { label: localized(language, 'Weekly report', '每周报告'), owner: seed % 2 ? 'Mina' : 'Omar', status: seed % 2 ? localized(language, 'Review', '审核') : localized(language, 'Draft', '草稿'), selected: false },
        { label: localized(language, 'Weekly report', '每周报告'), owner: seed % 2 ? 'Omar' : 'Mina', status: seed % 2 ? localized(language, 'Draft', '草稿') : localized(language, 'Review', '审核'), selected: false },
      ] } },
      truth: { selected_context: { owner: seed % 2 ? 'Mina' : 'Omar', status: seed % 2 ? localized(language, 'Review', '审核') : localized(language, 'Draft', '草稿') }, unchanged_other_row: true },
    }),
  },
  {
    id: 'LIST-03',
    scope: language => ({ list: localized(language, 'Archive', '归档'), target: localized(language, 'Archived item', '已归档项目') }),
    request: (language, seed) => ({
      objective: localized(language, `Select Archived item ${100 + seed} from the Archive list.`, `从“归档”列表中选择“已归档项目 ${100 + seed}”。`),
      initial: { list: { virtualized: true, initially_rendered_rows: 20, target_accessible: true } },
      truth: { required_selected_row: `archived_item_${100 + seed}`, target_position_after_20: true },
    }),
  },
  {
    id: 'LIST-04',
    scope: language => ({ list: localized(language, 'Records', '记录'), target: localized(language, 'North team record', '北区团队记录') }),
    request: (language, seed) => ({
      objective: localized(language, `Set the status of the North team record ${seedCode(seed)} to Reviewed. The list is sorted by most recently updated.`, `将“北区团队记录 ${seedCode(seed)}”的状态设为“已审核”。列表按最近更新时间排序。`),
      initial: { list: { sort: 'updated_descending', target: { label: `${localized(language, 'North team record', '北区团队记录')} ${seedCode(seed)}`, status: localized(language, 'Open', '未处理'), updated_order: 3 }, neighboring_rows: 4 } },
      truth: { required_final_state: { target_status: localized(language, 'Reviewed', '已审核') }, row_identity_must_survive_reorder: true },
    }),
  },
  {
    id: 'MODAL-01',
    scope: language => ({ modal: localized(language, 'Session notice', '会话提示'), background: 'unchanged' }),
    request: language => ({
      objective: localized(language, 'Dismiss the known Session notice so its blocked page is available. Do not change the page behind it.', '关闭已知的“会话提示”，使其遮挡的页面可用。不要更改弹窗后面的页面。'),
      initial: { active_modal: { title: localized(language, 'Session notice', '会话提示'), kind: 'known_informational' }, background_input: 'must_remain_unchanged' },
      truth: { modal_open: false, background_unchanged: true },
    }),
  },
  {
    id: 'MODAL-02',
    scope: language => ({ modal: localized(language, 'Create reminder', '创建提醒'), control: localized(language, 'Reminder category', '提醒类别') }),
    request: (language, seed) => {
      const value = localized(language, seed % 2 ? 'Personal' : 'Work', seed % 2 ? '个人' : '工作');
      return { objective: localized(language, `Set Reminder category inside the open Create reminder dialog to ${value}.`, `将当前“创建提醒”对话框内的“提醒类别”设为${value}。`),
        initial: { active_modal: { title: localized(language, 'Create reminder', '创建提醒') }, controls: [{ label: localized(language, 'Reminder category', '提醒类别'), value: localized(language, 'Other', '其他') }] },
        truth: { modal_control: { reminder_category: value }, modal_remains_open: true } };
    },
  },
  {
    id: 'MODAL-03',
    scope: language => ({ modal: localized(language, 'Confirm preferences', '确认偏好设置'), background: 'read_only' }),
    request: language => ({
      objective: localized(language, 'Choose Keep current selection in the open Confirm preferences dialog. Do not interact with any control behind the dialog.', '在打开的“确认偏好设置”对话框中选择“保留当前选择”。不要操作对话框后面的任何控件。'),
      initial: { active_modal: { title: localized(language, 'Confirm preferences', '确认偏好设置') }, background_controls: [{ label: localized(language, 'Background alerts', '背景提醒'), value: false }] },
      truth: { modal_choice: 'keep_current_selection', background_controls_unchanged: true },
    }),
  },
];

assert.deepEqual(fixtureSpecs.map(spec => spec.id), fixtureTaskIds, 'fixture task definitions must match the frozen report IDs');
assert.equal(fixtureTaskIds.length, FIXTURE_TASK_COUNT, 'fixture task count differs from the frozen denominator');
assert.equal(fixtureTaskIds.length * FIXTURE_SEEDS.length, FIXTURE_RUN_COUNT, 'fixture run count differs from the frozen denominator');
assert.equal(textEditTaskIds.length, TEXTEDIT_RUN_COUNT, 'TextEdit run count differs from the frozen denominator');

function makeFixtureProfile(spec, taskIndex, seed, fixtureApp) {
  const variants = assignTaskVariants(taskIndex, seed);
  const language = variants.language;
  const content = spec.request(language, seed);
  const allowed = spec.scope(language, seed);
  const initial = typeof content.initial === 'function' ? content.initial(language, seed) : content.initial;
  const initialState = {
    application: copy(fixtureApp),
    variant: copy(variants),
    ...copy(initial),
  };
  const request = {
    objective: content.objective,
    allowed_scope: { application: fixtureApp.name, targets: [allowed], no_unrelated_controls: true },
    initial_state: initialState,
  };
  const truth = {
    ...copy(content.truth),
    evidence: ['independent_post_state', 'required_persistence_receipt_when_specified'],
    forbidden_side_effects: ['unrelated_controls_changed', 'background_modal_controls_changed'],
  };
  return {
    online: { suite: 'fixture', task_id: spec.id, seed, task_request: request },
    truth: { suite: 'fixture', task_id: spec.id, seed, evaluator_truth: truth },
  };
}

const existingTextEditIds = new Set(['TEXTEDIT-11', 'TEXTEDIT-12', 'TEXTEDIT-13', 'TEXTEDIT-14', 'TEXTEDIT-15',
  'TEXTEDIT-16', 'TEXTEDIT-17', 'TEXTEDIT-18', 'TEXTEDIT-19', 'TEXTEDIT-20']);

function makeTextEditProfile(id, taskIndex, outputDirectory, textEditApp) {
  const language = taskIndex % 2 === 0 ? 'en-US' : 'zh-CN';
  const variants = {
    language,
    theme: THEME_NAMES[(taskIndex + Math.floor(taskIndex / 3)) % 2],
    layout: LAYOUTS[(taskIndex * 3) % LAYOUTS.length],
  };
  const existing = existingTextEditIds.has(id);
  const fileName = `cu-mvp-${id.toLowerCase()}.txt`;
  const targetContent = language === 'zh-CN'
    ? `Lingxi TextEdit 验证 ${id}。\n这是要求保存并重新打开读取的纯文本内容。\n`
    : `Lingxi TextEdit verification ${id}.\nThis is the plain-text content to save and read back.\n`;
  const oldContent = `Disposable initial fixture content for ${id}.\n`;
  const objective = language === 'zh-CN'
    ? `在 TextEdit 中，将“${outputDirectory}/${fileName}”保存为纯文本，内容精确为：\n${targetContent}${existing ? '该路径是本次专用的可丢弃测试文件；只在覆盖确认明确获准后替换它。' : ''}`
    : `In TextEdit, save a plain-text file at “${outputDirectory}/${fileName}” with exactly this content:\n${targetContent}${existing ? 'This is a disposable file created for this run; replace only this named test file after explicit overwrite confirmation.' : ''}`;
  const initialState = {
    application: copy(textEditApp),
    variant: copy(variants),
    output_directory: outputDirectory,
    file_name: fileName,
    document_format: 'plain_text',
    file_state: existing ? 'existing_disposable_test_file' : 'target_file_absent',
    document_state: existing ? 'existing_file_open' : 'new_untitled_document',
    initial_contents: existing ? oldContent : null,
    save_dialog_variant: existing ? 'existing_file_overwrite_confirmation' : 'first_save_native_dialog',
  };
  const online = {
    suite: 'textedit',
    task_id: id,
    seed: TEXTEDIT_SEED,
    task_request: {
      objective,
      allowed_scope: {
        application: textEditApp.name,
        output_directory: outputDirectory,
        file_name: fileName,
        document_format: 'plain_text',
        operations: ['write_requested_contents', 'save', 'close', 'reopen_and_read_back'],
        no_other_files: true,
      },
      initial_state: initialState,
    },
  };
  const overwrite = existing ? {
    required: true,
    confirmation_id: `overwrite-${id.toLowerCase()}`,
    target_file_name: fileName,
    driver_answer: 'approve',
    actual_confirmation_event_required: true,
    event_fields: ['confirmation_id', 'target_file_name', 'response', 'observed_at'],
    initial_file_sha256: sha256(Buffer.from(oldContent, 'utf8')),
    plan_status: 'not_run_no_confirmation_event_yet',
  } : { required: false, actual_confirmation_event_required: false, plan_status: 'not_applicable' };
  const truth = {
    suite: 'textedit',
    task_id: id,
    seed: TEXTEDIT_SEED,
    evaluator_truth: {
      expected_file_name: fileName,
      expected_format: 'plain_text',
      expected_content_sha256: sha256(Buffer.from(targetContent, 'utf8')),
      expected_content_byte_length: Buffer.byteLength(targetContent, 'utf8'),
      expected_ui_close_reopen_readback: true,
      expected_initial_file_state: initialState.file_state,
      destructive_overwrite_confirmation: overwrite,
      evaluator_must_read_saved_file_independently: true,
    },
  };
  return { online, truth };
}

function within(root, candidate) {
  const rel = relative(root, candidate);
  return rel === '' || (rel !== '..' && !rel.startsWith(`..${sep}`) && !isAbsolute(rel));
}

function sameStat(a, b) {
  return a.dev === b.dev && a.ino === b.ino && a.size === b.size
    && a.mtimeMs === b.mtimeMs && a.ctimeMs === b.ctimeMs;
}

export function hashBoundedAsset(rootPath, asset) {
  nonempty(rootPath, 'asset_root');
  assert.ok(isAbsolute(rootPath), 'asset_root must be absolute');
  const assetRoot = realpathSync(rootPath);
  assert.ok(statSync(assetRoot).isDirectory(), 'asset_root must be a directory');
  assert.ok(asset && typeof asset === 'object' && HEX_SHA256.test(asset.sha256), 'declared artifact hash is required');
  exactKeys(asset, ['relative_path', 'sha256', 'max_bytes'], 'asset descriptor');
  const relativePath = nonempty(asset.relative_path, 'asset relative path', 2048);
  assert.ok(!isAbsolute(relativePath) && !relativePath.split(/[\\/]/).includes('..'), 'asset path must stay within asset_root');
  assert.ok(Number.isSafeInteger(asset.max_bytes) && asset.max_bytes > 0 && asset.max_bytes <= MAX_ASSET_BYTES, 'asset read bound is invalid');
  const file = resolve(assetRoot, relativePath);
  assert.ok(within(assetRoot, file), 'asset path escapes asset_root');
  const before = lstatSync(file);
  assert.ok(before.isFile() && !before.isSymbolicLink() && before.nlink === 1, 'asset must be a single regular file');
  assert.ok(before.size > 0 && before.size <= asset.max_bytes, 'asset exceeds its declared read bound');
  assert.equal(realpathSync(file), file, 'asset path contains a symbolic link');

  let fd;
  const digest = createHash('sha256');
  const buffer = Buffer.allocUnsafe(64 * 1024);
  let bytesRead = 0;
  try {
    fd = openSync(file, constants.O_RDONLY | constants.O_NOFOLLOW);
    const opened = fstatSync(fd);
    assert.ok(opened.isFile() && sameStat(before, opened) && opened.nlink === 1, 'asset changed before hashing');
    while (bytesRead < opened.size) {
      const count = readSync(fd, buffer, 0, Math.min(buffer.length, opened.size - bytesRead), bytesRead);
      assert.ok(count > 0, 'asset ended before its recorded size');
      digest.update(buffer.subarray(0, count));
      bytesRead += count;
    }
    const after = fstatSync(fd);
    assert.ok(bytesRead === opened.size && sameStat(opened, after) && sameStat(opened, lstatSync(file)), 'asset changed while hashing');
  } finally {
    buffer.fill(0);
    if (fd !== undefined) closeSync(fd);
  }
  const actual = digest.digest('hex');
  assert.equal(actual, asset.sha256, 'declared artifact hash does not match the bounded file read');
  return actual;
}

export function validateAndHashEnvironmentManifest(environment) {
  exactKeys(environment, ['version', 'os', 'display', 'power_mode', 'hardware', 'app', 'textedit_app', 'model', 'graph', 'preprocess', 'program', 'knowledge', 'asset_root', 'artifact_assets'], 'environment manifest');
  assert.equal(environment.version, 1, 'environment manifest version must be 1');
  exactKeys(environment.os, ['name', 'build'], 'OS manifest');
  const os = { name: nonempty(environment.os.name, 'OS name'), build: nonempty(environment.os.build, 'OS build') };
  assert.equal(os.name, 'macOS', 'task-profile environment must identify macOS');
  exactKeys(environment.display, ['width_px', 'height_px', 'dpi_x', 'dpi_y'], 'display manifest');
  const display = {
    width_px: positiveInteger(environment.display.width_px, 'display width', 32768),
    height_px: positiveInteger(environment.display.height_px, 'display height', 32768),
    dpi_x: finitePositive(environment.display.dpi_x, 'horizontal DPI', 2400),
    dpi_y: finitePositive(environment.display.dpi_y, 'vertical DPI', 2400),
  };
  const powerMode = nonempty(environment.power_mode, 'power mode', 128);
  exactKeys(environment.hardware, ['model', 'architecture', 'memory_gib'], 'hardware manifest');
  const hardware = {
    model: nonempty(environment.hardware.model, 'hardware model'),
    architecture: nonempty(environment.hardware.architecture, 'hardware architecture'),
    memory_gib: positiveInteger(environment.hardware.memory_gib, 'hardware memory', 4096),
  };
  exactKeys(environment.app, ['name', 'bundle_id', 'version'], 'application manifest');
  const app = {
    name: nonempty(environment.app.name, 'application name'),
    bundle_id: nonempty(environment.app.bundle_id, 'application bundle ID'),
    version: nonempty(environment.app.version, 'application version'),
  };
  exactKeys(environment.textedit_app, ['name', 'bundle_id', 'version'], 'TextEdit application manifest');
  const texteditApp = {
    name: nonempty(environment.textedit_app.name, 'TextEdit application name'),
    bundle_id: nonempty(environment.textedit_app.bundle_id, 'TextEdit application bundle ID'),
    version: nonempty(environment.textedit_app.version, 'TextEdit application version'),
  };
  assert.equal(texteditApp.bundle_id, 'com.apple.TextEdit', 'TextEdit application bundle ID must be fixed');
  exactKeys(environment.model, ['name', 'version'], 'model manifest');
  const model = { name: nonempty(environment.model.name, 'model name'), version: nonempty(environment.model.version, 'model version') };
  const release = (key, label) => {
    exactKeys(environment[key], ['release'], `${label} manifest`);
    return { release: nonempty(environment[key].release, `${label} release`) };
  };
  const graph = release('graph', 'graph');
  const preprocess = release('preprocess', 'preprocess');
  const program = release('program', 'program');
  exactKeys(environment.knowledge, ['release', 'task_workflows_included'], 'knowledge manifest');
  const knowledge = {
    release: nonempty(environment.knowledge.release, 'knowledge release'),
    task_workflows_included: environment.knowledge.task_workflows_included,
  };
  assert.equal(knowledge.task_workflows_included, false, 'cold-start freeze cannot include task-specific workflows');
  assert.ok(isAbsolute(environment.asset_root), 'asset_root must be absolute');
  assert.ok(environment.artifact_assets && typeof environment.artifact_assets === 'object'
    && !Array.isArray(environment.artifact_assets), 'artifact assets are required');
  const artifactNames = Object.keys(environment.artifact_assets).sort();
  assert.ok(artifactNames.length >= ARTIFACT_NAMES.length && artifactNames.length <= 128
    && ARTIFACT_NAMES.every(name => artifactNames.includes(name))
    && artifactNames.every(name => /^[a-z][a-z0-9_-]{0,63}$/.test(name)), 'artifact asset set is incomplete or invalid');
  const declaredAssetReadBytes = artifactNames.reduce((total, name) => total + environment.artifact_assets[name]?.max_bytes, 0);
  assert.ok(Number.isSafeInteger(declaredAssetReadBytes) && declaredAssetReadBytes <= MAX_TOTAL_ASSET_READ_BYTES,
    'aggregate artifact read bound is invalid');
  const artifactHashes = Object.fromEntries(artifactNames.map(name => [
    name, hashBoundedAsset(environment.asset_root, environment.artifact_assets[name]),
  ]));
  const sanitized = deepFreeze({ version: 1, os, display, power_mode: powerMode, hardware, app, textedit_app: texteditApp, model, graph, preprocess,
    program, knowledge, artifact_hashes: artifactHashes });
  return { manifest: sanitized, sha256: canonicalSha256(sanitized) };
}

function checkNoForbiddenRequestFields(value, location = 'request') {
  if (Array.isArray(value)) { value.forEach((item, index) => checkNoForbiddenRequestFields(item, `${location}[${index}]`)); return; }
  if (!value || typeof value !== 'object') return;
  for (const [key, child] of Object.entries(value)) {
    assert.ok(!REQUEST_FORBIDDEN_KEY.test(key), `${location} contains evaluator or navigation material`);
    checkNoForbiddenRequestFields(child, `${location}.${key}`);
  }
}

export function buildTaskProfileBundle({ environment, taskOutputDirectory }) {
  assert.ok(typeof taskOutputDirectory === 'string' && isAbsolute(taskOutputDirectory), 'task output directory must be an explicit absolute path');
  assert.ok(!taskOutputDirectory.split(/[\\/]/).includes('..'), 'task output directory cannot contain parent traversal');
  const outputDirectory = resolve(taskOutputDirectory);
  const { manifest: frozenEnvironment, sha256: environmentSha256 } = validateAndHashEnvironmentManifest(environment);
  const fixtures = fixtureSpecs.flatMap((spec, index) => FIXTURE_SEEDS.map(seed => makeFixtureProfile(spec, index, seed, frozenEnvironment.app)));
  const textEdits = textEditTaskIds.map((id, index) => makeTextEditProfile(id, index, outputDirectory, frozenEnvironment.textedit_app));
  const onlineRequests = {
    format_version: 1,
    task_profile_version: TASK_PROFILE_VERSION,
    records: [...fixtures.map(item => item.online), ...textEdits.map(item => item.online)],
  };
  const evaluatorTruth = {
    format_version: 1,
    task_profile_version: TASK_PROFILE_VERSION,
    audience: 'independent_evaluator_only',
    not_for_online_model: true,
    records: [...fixtures.map(item => item.truth), ...textEdits.map(item => item.truth)],
  };
  checkNoForbiddenRequestFields(onlineRequests);
  const requestBytes = Buffer.from(stableJson(onlineRequests), 'utf8');
  const truthBytes = Buffer.from(stableJson(evaluatorTruth), 'utf8');
  const freezeBase = {
    format_version: 1,
    task_profile_version: TASK_PROFILE_VERSION,
    plan_only: true,
    execution_started: false,
    business_results: 'not_run',
    environment: frozenEnvironment,
    environment_sha256: environmentSha256,
    denominators: { fixture_tasks: FIXTURE_TASK_COUNT, fixture_seeds_per_task: 5, fixture_runs: FIXTURE_RUN_COUNT, textedit_runs: TEXTEDIT_RUN_COUNT, total_records: onlineRequests.records.length },
    export_hashes: { online_requests_sha256: sha256(requestBytes), evaluator_truth_sha256: sha256(truthBytes) },
  };
  const freezeManifest = { ...freezeBase, freeze_content_sha256: canonicalSha256(freezeBase) };
  const files = {
    'task-requests.json': requestBytes,
    'evaluator-truth.json': truthBytes,
    'task-profile-freeze.json': Buffer.from(stableJson(freezeManifest), 'utf8'),
  };
  const goalCatalog = buildFrozenGoalCatalog({
    requestBytes,
    freezeBytes: files['task-profile-freeze.json'],
    projectGoal: projectTaskRequestForModel,
  });
  return deepFreeze({ onlineRequests, evaluatorTruth, freezeManifest, files,
    goalCatalog,
    hashes: Object.fromEntries(Object.entries(files).map(([name, bytes]) => [name, sha256(bytes)])) });
}

/**
 * Project the model/Goal input from one driver record. Driver reset state is
 * intentionally omitted: Environment must come from the live Source snapshot.
 */
export function projectTaskRequestForModel(record) {
  assert.ok(record && typeof record === 'object' && record.task_request
    && typeof record.task_request === 'object', 'task request record is required');
  const request = record.task_request;
  exactKeys(request, ['objective', 'allowed_scope', 'initial_state'], 'driver task request');
  validateObjective(request.objective);
  assert.ok(request.allowed_scope && typeof request.allowed_scope === 'object' && !Array.isArray(request.allowed_scope), 'allowed scope is required');
  const projection = { objective: request.objective, allowed_scope: copy(request.allowed_scope) };
  checkNoForbiddenRequestFields(projection, 'model projection');
  return deepFreeze(projection);
}

/** Projects every driver profile without carrying reset state or evaluator data. */
export function projectTaskRequestsForModel(requestExport) {
  assert.ok(requestExport && requestExport.format_version === 1 && Array.isArray(requestExport.records), 'task request export is invalid');
  return deepFreeze(requestExport.records.map(projectTaskRequestForModel));
}

export function writeFrozenTaskProfiles(outputDirectory, bundle) {
  assert.ok(typeof outputDirectory === 'string' && isAbsolute(outputDirectory), 'artifact output directory must be absolute');
  const outputStat = lstatSync(outputDirectory);
  assert.ok(outputStat.isDirectory() && !outputStat.isSymbolicLink(), 'artifact output must be a real directory');
  const fileNames = Object.keys(bundle.files).sort();
  assert.deepEqual(fileNames, ['evaluator-truth.json', 'task-profile-freeze.json', 'task-requests.json'], 'frozen artifact set is invalid');
  assert.ok(bundle.goalCatalog?.files && typeof bundle.goalCatalog.files === 'object', 'goal-only catalog is required');
  const goalCatalogFileNames = Object.keys(bundle.goalCatalog.files).sort();
  assert.deepEqual(goalCatalogFileNames, ['goal-catalog-manifest.json', 'task-goals.json'], 'goal-only catalog artifact set is invalid');
  for (const name of fileNames) {
    assert.ok(Buffer.isBuffer(bundle.files[name]), `frozen artifact ${name} bytes are invalid`);
    assert.equal(sha256(bundle.files[name]), bundle.hashes?.[name], `frozen artifact ${name} changed after validation`);
  }
  for (const name of goalCatalogFileNames) {
    assert.ok(Buffer.isBuffer(bundle.goalCatalog.files[name]), `goal catalog ${name} bytes are invalid`);
    assert.equal(sha256(bundle.goalCatalog.files[name]), bundle.goalCatalog.hashes?.[name], `goal catalog ${name} changed after validation`);
  }
  assert.equal(bundle.goalCatalog.catalogId, sha256(bundle.goalCatalog.files['goal-catalog-manifest.json']), 'goal catalog ID does not match manifest bytes');
  assert.equal(bundle.goalCatalog.manifest.task_goals_sha256, sha256(bundle.goalCatalog.files['task-goals.json']), 'goal catalog manifest does not bind goal bytes');
  assert.equal(bundle.goalCatalog.manifest.source_request_sha256, sha256(bundle.files['task-requests.json']), 'goal catalog does not bind frozen requests');
  assert.equal(bundle.goalCatalog.manifest.source_freeze_sha256, sha256(bundle.files['task-profile-freeze.json']), 'goal catalog does not bind source freeze');
  const allFileNames = [...fileNames, ...goalCatalogFileNames].sort();
  for (const name of allFileNames) {
    assert.throws(() => lstatSync(resolve(outputDirectory, name)), { code: 'ENOENT' }, 'output contains an already frozen artifact');
  }
  const written = [];
  try {
    for (const name of allFileNames) {
      const target = resolve(outputDirectory, name);
      const bytes = bundle.files[name] ?? bundle.goalCatalog.files[name];
      assert.ok(Buffer.isBuffer(bytes), 'frozen artifact bytes are invalid');
      // wx gives every frozen artifact a unique immutable destination.
      const fd = openSync(target, 'wx', 0o600);
      const identity = fstatSync(fd);
      written.push({ name, dev: identity.dev, ino: identity.ino });
      try {
        let offset = 0;
        while (offset < bytes.length) {
          const count = writeSync(fd, bytes, offset, bytes.length - offset, offset);
          assert.ok(count > 0, 'frozen artifact write made no progress');
          offset += count;
        }
        fsyncSync(fd);
      } finally { closeSync(fd); }
    }
  } catch (error) {
    // Remove only files created by this exact attempt, without touching an
    // existing frozen destination. Identity is rechecked before each unlink.
    for (const item of written.reverse()) {
      const target = resolve(outputDirectory, item.name);
      try {
        const stat = lstatSync(target);
        if (stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1
          && stat.dev === item.dev && stat.ino === item.ino) unlinkSync(target);
      } catch { /* Leave uncertain files for manual inspection. */ }
    }
    throw error;
  }
  return {
    files: fileNames,
    hashes: copy(bundle.hashes),
    goalCatalogFiles: goalCatalogFileNames,
    goalCatalogHashes: copy(bundle.goalCatalog.hashes),
    catalogId: bundle.goalCatalog.catalogId,
    frozen_without_overwrite: true,
  };
}

export const fixtureProfileIds = fixtureTaskIds;
export const textEditProfileIds = textEditTaskIds;
export const taskProfileLayoutNames = LAYOUTS;
