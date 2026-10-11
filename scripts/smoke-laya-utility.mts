import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { build } from 'esbuild';

// No Host, Device, document, credentials, provider request, or physical input.
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const { values } = parseArgs({ options: { runtime: { type: 'string' } } });
assert.ok(values.runtime && isAbsolute(values.runtime) && existsSync(values.runtime));
const runtime = values.runtime;
const modelRevision = 'e4e9ddf21a7b1903b7acffd8814ad4307bf63a67';
const workerModulePath = join(root, 'apps/desktop/out/main/localDecisionWorker.js');
const bundleManifestPath = join(root, 'target/model-exports/laya-multilingual', modelRevision, 'package-manifest.json');
assert.ok(existsSync(workerModulePath) && existsSync(bundleManifestPath));
const output = mkdtempSync(join(root, 'target/laya-utility-smoke-'));
const manualProofPath = join(output, 'result.json');
const entryPath = join(output, 'main.cjs');
const artifact = join(root, 'artifacts', `m0-laya-utility-${randomUUID()}.json`);
const sha256 = (path: string): string => createHash('sha256').update(readFileSync(path)).digest('hex');
const pins = () => ({ worker: sha256(workerModulePath), manifest: sha256(bundleManifestPath),
  lifecycle: sha256(join(root, 'apps/desktop/src/main/localDecisionProcess.ts')),
  protocol: sha256(join(root, 'packages/decision-worker/src/utilityProtocol.ts')),
  worker_source: sha256(join(root, 'packages/decision-worker/src/utilityWorker.ts')),
  teacher_observer: sha256(join(root, 'apps/desktop/src/main/teacherShadowObserver.ts')),
  build_chunks: Object.fromEntries(readdirSync(join(root, 'apps/desktop/out/main/chunks')).sort()
    .filter(name => name.endsWith('.js')).map(name => [name, sha256(join(root, 'apps/desktop/out/main/chunks', name))])),
  backend: sha256(join(root, 'packages/decision-worker/src/laya/layaBackend.ts')) });
const before = pins();
const source = `
import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import { performance } from 'node:perf_hooks';
import { app, utilityProcess } from 'electron';
import { LocalDecisionProcess } from ${JSON.stringify(join(root, 'apps/desktop/src/main/localDecisionProcess.ts'))};
import { TeacherObservationDecoder, TeacherShadowObserver } from ${JSON.stringify(join(root, 'apps/desktop/src/main/teacherShadowObserver.ts'))};
const proof = { ok:false, checks:{}, real_child_processes:0, child_exits:0, provider_calls:0, physical_inputs:0,
  runtime_version:process.versions.electron,node_version:process.versions.node,platform:process.platform,architecture:process.arch,
  authoritative_product_backend_enabled:false, task_quality_verified:false, shared_load_verified:false };
let runner; let stage='startup';
const children=[]; const closed=[];
const nowMs=()=>Math.floor(performance.now());
const request=(id,generation=1)=>{
  const candidates=['c1','c2'].map(candidate_id=>({candidate_id,skill:'ui.ensure_value',semantic_target:null,
    binding:null,parameters:{},effect_class:'reversible_edit',precondition:{state:'executable'},description:'Select '+candidate_id}));
  const now=nowMs();
  return {decision_id:id,lane_id:'lane-smoke',request_generation:generation,root_task_id:'root-smoke',
    task_revision:1,plan_revision:1,root_epoch:1,decision_program:'ui.step@1',calibration_revision:'none',
    created_ms:now,result_deadline_ms:now+30000,state:{root_task_id:'root-smoke',task_revision:1,
      goal_summary:'choose one listed action',subgoal:'select an action',snapshot_revision:1,objects:[],
      candidates,recent_results:[],missing_fields:[],omitted_items:[],candidate_set_digest:'cs1:utility-smoke'},
    questions:[{id:'q1',kind:'choice',prompt:'Which listed action should run?',
      options:candidates.map(c=>({key:c.candidate_id,description:c.description}))}]};
};
const check=(name,value)=>{assert.ok(value,name);proof.checks[name]=true;};
const start=()=>LocalDecisionProcess.start({workerModulePath:${JSON.stringify(workerModulePath)},
  bundleManifestPath:${JSON.stringify(bundleManifestPath)},numThreads:2,nowMs,
  spawnUtilityProcess:(module,args,options)=>{
    const child=utilityProcess.fork(module,args,options);children.push(child);
    proof.real_child_processes++;child.on('spawn',()=>check('distinct_child_pid',Number.isInteger(child.pid)&&child.pid!==process.pid));
    closed.push(new Promise(resolve=>child.once('exit',()=>{proof.child_exits++;resolve();})));return child;
  }});
const watchdog=setTimeout(()=>{stage='watchdog';for(const child of children)child.kill();
  writeFileSync(${JSON.stringify(manualProofPath)},JSON.stringify({...proof,failure_stage:stage}));app.exit(1);},55000);
app.whenReady().then(async()=>{
  try{
    stage='load';check('pinned_electron',process.versions.electron==='43.1.1');
    runner=await start();proof.startup_failure_code=runner.unavailableCode;check('ready',runner.availability==='ready');
    check('pinned_model',runner.loadedModelRevision===${JSON.stringify(modelRevision)});
    stage='request';const input=request('first');const answer=await runner.submit(input);
    check('s1_proposal',answer.kind==='proposal'&&answer.proposal.source==='s1');
    check('correlated_identity',answer.kind==='proposal'&&answer.proposal.decision_id===input.decision_id
      &&answer.proposal.candidate_set_digest===input.state.candidate_set_digest);
    check('inference_or_confidence_refusal',answer.kind==='proposal'
      &&(answer.proposal.failure===null||answer.proposal.failure==='refused'));
    proof.proposal_failure=answer.proposal.failure;
    stage='cancel';const cancel=request('cancel');const cancelled=runner.submit(cancel);
    const cancelStarted=performance.now();runner.cancel({decision_id:cancel.decision_id,request_generation:1});
    const cancelResult=await cancelled;proof.cancel_delivery_ms=performance.now()-cancelStarted;
    check('cancel_drops',cancelResult.kind==='dropped'&&cancelResult.reason==='cancelled');
    check('cancel_delivery_under_150ms',proof.cancel_delivery_ms<=150);
    stage='invalidate';const stale=runner.submit(request('invalidate'));runner.invalidate();
    check('epoch_drops',(await stale).kind==='dropped');
    stage='close';const closeStarted=performance.now();await runner.close();await closed[0];
    proof.close_ms=performance.now()-closeStarted;check('closed_child_exited',proof.child_exits===1);
    check('closed_no_late_delivery',(await runner.submit(request('closed'))).kind==='dropped');
    stage='worker_exit';runner=await start();check('second_ready',runner.availability==='ready');
    const pending=runner.submit(request('crash'));children[1].kill();await closed[1];
    check('death_drops_pending',(await pending).kind==='dropped');
    check('death_terminal',runner.availability==='unavailable');
    const afterDeath=await runner.submit(request('after-death'));
    check('death_returns_unavailable',afterDeath.kind==='proposal'&&afterDeath.proposal.failure==='unavailable'
      &&afterDeath.proposal.choice===null&&afterDeath.proposal.source==='s1');
    check('no_automatic_restart',proof.real_child_processes===2);await runner.close();
    stage='teacher_shadow';runner=await start();check('teacher_shadow_ready',runner.availability==='ready');
    const observer=new TeacherShadowObserver(runner,{nowMs});
    const decoder=new TeacherObservationDecoder();const shadowInput=request('teacher-shadow');
    const scope={decision_id:shadowInput.decision_id,request_generation:shadowInput.request_generation,
      root_task_id:shadowInput.root_task_id,root_epoch:shadowInput.root_epoch,task_revision:shadowInput.task_revision,
      candidate_set_digest:shadowInput.state.candidate_set_digest};
    const teacherProposal={decision_id:scope.decision_id,request_generation:scope.request_generation,
      candidate_set_digest:scope.candidate_set_digest,backend:'fixture-teacher',source:'teacher',model_revision:'fixture',
      answers:[{question_id:'q1',selected:'c1',confidence_milli:1000}],choice:{action:'start_skill',candidate:'c1'},
      failure:null,truncated:false,queue_ms:0,inference_ms:1};
    const feed=message=>{const body=Buffer.from(JSON.stringify(message));const prefix=Buffer.alloc(4);
      prefix.writeUInt32BE(body.length);const frame=Buffer.concat([prefix,body]);
      for(const piece of [frame.subarray(0,2),frame.subarray(2)])for(const decoded of decoder.push(piece))observer.observe(decoded);};
    feed({version:1,type:'started',request:shadowInput});
    feed({version:1,type:'finished',scope,proposal:teacherProposal});
    const shadowDeadline=performance.now()+10000;
    while(observer.statistics.in_flight>0&&performance.now()<shadowDeadline)await new Promise(resolve=>setTimeout(resolve,10));
    const stats=observer.statistics;proof.teacher_shadow_statistics=stats;
    proof.teacher_result_is_synthetic=true;proof.real_teacher_provider_verified=false;
    check('teacher_shadow_started',stats.started===1);
    check('teacher_shadow_settled',stats.in_flight===0&&(stats.verified+stats.refused)>0);
    check('teacher_shadow_did_not_emit_input',proof.physical_inputs===0);
    await observer.close();await runner.close();await closed[2];decoder.dispose();
    check('all_three_children_exited',proof.child_exits===3);proof.ok=true;
  }catch{proof.failure_stage=stage;}
  finally{await runner?.close().catch(()=>undefined);clearTimeout(watchdog);
    writeFileSync(${JSON.stringify(manualProofPath)},JSON.stringify(proof));app.exit(proof.ok?0:1);}
});
`;
let timedOut = false;
let exitCode: number | null = null;
try {
  await build({ stdin: { contents: source, resolveDir: root, sourcefile: 'layaUtilitySmoke.ts', loader: 'ts' },
    bundle: true, platform: 'node', target: 'node22', format: 'cjs', outfile: entryPath,
    external: ['electron', 'onnxruntime-node'], logLevel: 'silent' });
  const env: NodeJS.ProcessEnv = {};
  for (const key of ['PATH', 'HOME', 'TMPDIR', 'LANG', 'LC_ALL']) if (process.env[key]) env[key] = process.env[key];
  const child = spawn(runtime, [entryPath], { cwd: root, env, stdio: 'ignore' });
  exitCode = await new Promise<number | null>((done) => {
    const timer = setTimeout(() => { timedOut = true; child.kill('SIGTERM'); }, 60_000);
    child.once('error', () => { clearTimeout(timer); done(null); });
    child.once('exit', code => { clearTimeout(timer); done(code); });
  });
  const manual = existsSync(manualProofPath) ? JSON.parse(readFileSync(manualProofPath, 'utf8')) : null;
  const after = pins();
  const unchanged = JSON.stringify(before) === JSON.stringify(after);
  const record = { kind: 'real_electron_laya_utility_and_teacher_observer', checked_at: new Date().toISOString(),
    ok: manual?.ok === true && exitCode === 0 && !timedOut && unchanged, runtime_sha256: sha256(runtime),
    runtime_version: manual?.runtime_version ?? null, pins_before: before, pins_after: after, pins_unchanged: unchanged,
    exit_code: exitCode, timed_out: timedOut, manual, mvp_complete: false };
  writeFileSync(artifact, JSON.stringify(record, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
  console.log(JSON.stringify({ artifact, ok: record.ok, failure_stage: manual?.failure_stage ?? null }));
  process.exitCode = record.ok ? 0 : 1;
} finally { rmSync(output, { recursive: true, force: true }); }
