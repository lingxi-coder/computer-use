/** Python `json.dumps` for a finite number. JavaScript prints the same shortest round-trip
digits, but Python switches to exponent notation below 1e-4 (JavaScript: below 1e-6) and pads the
exponent to two digits (`5e-05`, not `5e-5`). An integer-valued number prints as before, which
matches a Python int below 1e21: JavaScript cannot tell Python's `49.0` from `49`. */
function pyNumber(v) {
    const [mant, exp] = v.toExponential().split("e");
    const e = Number(exp);
    return e >= -4 ? String(v) : `${mant}e-${String(-e).padStart(2, "0")}`;
}
/** Python `json.dumps(v, ensure_ascii=False)` replica: separators (", ", ": "),
unicode raw, unknown types fall back to undefined (caller applies str()). */
function pyJson(v) {
    if (v === null)
        return "null";
    if (typeof v === "string" || typeof v === "boolean")
        return JSON.stringify(v);
    if (typeof v === "number") {
        if (Number.isNaN(v))
            return "NaN";
        if (v === Infinity)
            return "Infinity";
        if (v === -Infinity)
            return "-Infinity";
        return pyNumber(v);
    }
    if (Array.isArray(v))
        return `[${v.map((x) => pyJson(x) ?? "null").join(", ")}]`;
    if (typeof v === "object") {
        const proto = Object.getPrototypeOf(v);
        if (proto !== Object.prototype && proto !== null)
            return undefined;
        const parts = [];
        for (const [k, x] of Object.entries(v)) {
            const s = pyJson(x);
            if (s !== undefined)
                parts.push(`${JSON.stringify(k)}: ${s}`);
        }
        return `{${parts.join(", ")}}`;
    }
    return undefined;
}
export function serializeState(state) {
    if (typeof state === "string")
        return state;
    return pyJson(state) ?? String(state);
}
/** The text of a criterion: the string itself, else the JSON the model was shown.
 *
 * Exported because `agent.ts` needs it for a `score` answer's `legend`, which maps an index to the
 * text of that level. Putting the raw value in made the response's JSON types depend on the
 * caller's input -- a numeric scale came back as `{"0": 1}`, a boolean as `{"0": true}` and a null
 * as `{"0": null}`, which a Jev client refuses to parse (#302).
 */
export function renderCriterion(v) {
    return typeof v === "string" ? v : pyJson(v) ?? String(v);
}
export function renderOptions(q) {
    if (q.t === "choice") {
        const crit = q.crit;
        return Object.entries(crit).map(([k, v]) => v === null || v === undefined || v === "" ? k : `${k}: ${renderCriterion(v)}`);
    }
    if (q.t === "score") {
        return q.crit.map((c, i) => `level ${i}: ${renderCriterion(c)}`);
    }
    const crit = (q.crit ?? {});
    const labels = q.labels ?? { false: "false", true: "true" };
    const f = crit["false"], t = crit["true"];
    return [
        labels.false + ": " + (f !== null && f !== undefined && f !== "" ? renderCriterion(f) : "no, the statement does not hold"),
        labels.true + ": " + (t !== null && t !== undefined && t !== "" ? renderCriterion(t) : "yes, the statement holds"),
    ];
}
/** The question half of `buildSequence`: everything before the state tokens. Hoisted out so
 * callers asking several questions about the same state can encode the state text only once. */
// Per-tokenizer prefix cache (cap 1k, clear on overflow); repeat triage skips re-encode.
const prefixCache = new WeakMap();
export function buildQuestionPrefix(tok, q, maxLen = 512, headMaxLen = 192, optionOrder) {
    let per = prefixCache.get(tok);
    if (!per) {
        per = new Map();
        prefixCache.set(tok, per);
    }
    // Key on what actually builds the prefix: raw criteria lie (JSON.stringify
    // drops undefined values and throws on BigInt), rendered options don't.
    const key = JSON.stringify([q.t, q.ins, renderOptions(q), maxLen, headMaxLen, optionOrder ?? null]);
    const hit = per.get(key);
    if (hit)
        return hit;
    const built = buildQuestionPrefixUncached(tok, q, maxLen, headMaxLen, optionOrder);
    if (per.size > 1000)
        per.clear();
    per.set(key, built);
    return built;
}
function buildQuestionPrefixUncached(tok, q, maxLen = 512, headMaxLen = 192, optionOrder) {
    const maskTok = tok.maskToken;
    const opts = renderOptions(q);
    const order = optionOrder ?? opts.map((_, i) => i);
    const ins = String(q.ins).split(maskTok).join(" ");
    let headIds = tok.encode(`${q.t} question: ${ins}`);
    let optIds = order.map((i) => [tok.maskId, ...tok.encode(" " + opts[i].split(maskTok).join(" ")).slice(0, 48)]);
    let budget = headMaxLen - optIds.reduce((a, o) => a + o.length, 0);
    let tokensPerOption = null;
    if (budget < 16) {
        const per = Math.max(4, Math.floor((headMaxLen - 16) / Math.max(1, optIds.length)));
        optIds = optIds.map((o) => o.slice(0, per));
        budget = headMaxLen - optIds.reduce((a, o) => a + o.length, 0);
        tokensPerOption = per;
    }
    headIds = headIds.slice(0, Math.max(8, budget));
    const ids = [tok.clsId, ...headIds, tok.sepId];
    const markers = [];
    for (const o of optIds) {
        markers.push(ids.length);
        ids.push(...o);
    }
    ids.push(tok.sepId);
    // Counted on the capped option ids, before assembly, exactly as Python's `build_head` does:
    // re-slicing the finished sequence cannot close the last option's span, so the last option
    // always looks distinguishable however it collided (#538).
    return {
        ids, markers, nOptions: opts.length,
        optionStats: {
            total: opts.length,
            distinct: new Set(optIds.map((o) => o.join(","))).size,
            tokens_per_option: tokensPerOption,
        },
    };
}
/** Append pre-encoded state tokens to a question prefix. Identical output to building the
 * whole sequence in one pass, but the state only needs encoding once per state, not once
 * per (state, question) pair.
 *
 * The state is clamped to whatever room the head leaves; `stats` reports that clamp so callers
 * never have to guess it from the character length of what they sent (issue #174, Python #181). */
export function sequenceWithState(prefix, stateIds, sepId, maxLen = 512, truncateLeft = false) {
    const room = Math.max(0, maxLen - prefix.ids.length - 1);
    // not stateIds.slice(-room): with no room left, slice(-0) is the whole state rather than none of it
    const kept = truncateLeft ? stateIds.slice(Math.max(0, stateIds.length - room)) : stateIds.slice(0, room);
    const ids = [...prefix.ids, ...kept, sepId].slice(0, maxLen);
    // Count against the final clamp rather than `kept`: the clamp is what actually decided
    // which state tokens reached the encoder.
    const used = Math.max(0, Math.min(kept.length, maxLen - prefix.ids.length));
    return {
        ids,
        markers: prefix.markers.filter((m) => m < maxLen),
        stats: {
            state_tokens: stateIds.length,
            state_tokens_used: used,
            state_tokens_dropped: stateIds.length - used,
            truncated: used < stateIds.length,
            options: prefix.optionStats,
        },
    };
}
/** The questions whose options no longer have a token span each, keyed by question id.
 *
 * `total` is what the question defines, not the number of markers that reached the sequence, so a
 * report cannot say "43 of 43" about a question whose 28 missing options never entered the input
 * at all. Mirrors Python `laya.common.collapsed_options`; empty when nothing collapsed, which is
 * the overwhelming majority of requests, so the agents add the key only when it says something. */
export function collapsedOptions(qids, stats) {
    const out = {};
    qids.forEach((qid, i) => {
        const s = stats[i]?.options;
        if (s && s.distinct < s.total)
            out[qid] = { total: s.total, distinct: s.distinct,
                tokens_per_option: s.tokens_per_option };
    });
    return out;
}
export function buildSequence(tok, state, q, maxLen = 512, headMaxLen = 192, optionOrder, truncateLeft = false) {
    const stAll = tok.encode(serializeState(state).split(tok.maskToken).join(" "));
    return sequenceWithState(buildQuestionPrefix(tok, q, maxLen, headMaxLen, optionOrder), stAll, tok.sepId, maxLen, truncateLeft);
}
export function softmax(z) {
    const m = Math.max(...z);
    const e = z.map((v) => Math.exp(v - m));
    const s = e.reduce((a, b) => a + b, 0);
    return e.map((v) => v / s);
}
export function confidenceFromProbs(p) {
    const k = p.length;
    if (k < 2)
        return 1.0;
    const ent = -p.reduce((a, v) => a + v * Math.log(Math.max(v, 1e-12)), 0);
    return Math.min(1, Math.max(0, 1 - ent / Math.log(k)));
}
export function answerConfidence(p) {
    // Probability mass on the reported answer: max(p). This is the quantity temperature
    // scaling fits and the one every calibration figure is computed on, so it is stable
    // across option counts and comparable across question types -- unlike
    // confidenceFromProbs, whose entropy scale moves with k.
    if (p.length < 1)
        return 1.0;
    return Math.min(1, Math.max(0, Math.max(...p)));
}
function pyRepr(v) {
    if (typeof v === "boolean")
        return v ? "True" : "False";
    if (v === null || v === undefined)
        return "None";
    if (typeof v === "number") {
        if (Number.isNaN(v))
            return "nan";
        if (v === Infinity)
            return "inf";
        if (v === -Infinity)
            return "-inf";
        return String(v);
    }
    if (typeof v === "string")
        return JSON.stringify(v);
    if (Array.isArray(v))
        return `[${v.map(pyRepr).join(", ")}]`;
    if (typeof v === "object") {
        const entries = Object.entries(v).map(([k, val]) => `${pyRepr(k)}: ${pyRepr(val)}`);
        return `{${entries.join(", ")}}`;
    }
    return String(v);
}
const BUCKET_KEY = /^(choice|score|noul):(2|3-5|6-10|11\+)$/;
/**
 * Validate a per-bucket abstention-threshold map (#394).
 *
 * Keys are option-count bucket strings like "choice:2", "choice:3-5", "score:6-10", "noul:2",
 * plus an optional "default". Values are numbers in [0.0, 1.0].
 */
export function checkMinConfidenceMap(m) {
    if (!m || typeof m !== "object" || Array.isArray(m) || Object.keys(m).length === 0) {
        throw new Error(`a min_confidence map must be a non-empty dict of bucket -> float, got ${pyRepr(m)}`);
    }
    const out = {};
    for (const [key, val] of Object.entries(m)) {
        if (key !== "default" && !BUCKET_KEY.test(key)) {
            throw new Error(`min_confidence map keys must be strings like 'choice:3-5', got ${pyRepr(key)}`);
        }
        if (typeof val === "boolean" || typeof val !== "number" || !Number.isFinite(val) || val < 0.0 || val > 1.0) {
            throw new Error(`min_confidence must be a float in [0.0, 1.0], got ${pyRepr(val)}`);
        }
        out[key] = val;
    }
    return out;
}
/**
 * Validate opt-in abstention threshold `min_confidence` (#361, #394).
 *
 * Either a real number in [0.0, 1.0] or a per-bucket mapping. Booleans are rejected.
 */
export function checkMinConfidence(v) {
    if (v && typeof v === "object" && !Array.isArray(v)) {
        return checkMinConfidenceMap(v);
    }
    if (typeof v === "boolean" || typeof v !== "number" || !Number.isFinite(v) || v < 0.0 || v > 1.0) {
        throw new Error(`min_confidence must be a float in [0.0, 1.0], got ${pyRepr(v)}`);
    }
    return v;
}
export function optionBucket(answer) {
    const qt = answer.type;
    if (qt !== "choice" && qt !== "score" && qt !== "noul")
        return null;
    const probs = answer.probabilities;
    let k;
    if (probs && typeof probs === "object" && !Array.isArray(probs)) {
        k = Object.keys(probs).length;
    }
    else if (qt === "noul") {
        k = 2;
    }
    else {
        return null;
    }
    const size = k <= 2 ? "2" : k <= 5 ? "3-5" : k <= 10 ? "6-10" : "11+";
    return `${qt}:${size}`;
}
export function resolveMinConfidence(answer, thresholds, defaultVal = 0.0) {
    const key = optionBucket(answer);
    if (key !== null && key in thresholds) {
        return thresholds[key];
    }
    return thresholds.default ?? defaultVal;
}
/**
 * Opt-in abstention marker (#361, #394): flag answers whose confidence falls below `min_confidence`.
 *
 * Reads `answer_confidence` (the calibrated max(p) confidence, invariant to label count k),
 * falling back to `confidence` if `answer_confidence` is absent.
 * The raw answer and confidence stay intact; `low_confidence: true` is added.
 *
 * Supports both a scalar number in [0.0, 1.0] and a per-bucket mapping of thresholds.
 */
export function flagLowConfidence(results, minConfidence) {
    const isMap = typeof minConfidence === "object" && minConfidence !== null;
    if (!isMap && minConfidence === 0.0)
        return;
    const list = Array.isArray(results) ? results : [results];
    for (const res of list) {
        const answers = res && typeof res === "object" ? res.answers : null;
        if (!answers || typeof answers !== "object")
            continue;
        for (const a of Object.values(answers)) {
            if (!a || typeof a !== "object")
                continue;
            const ansObj = a;
            let conf = ansObj.answer_confidence;
            if (conf === undefined || conf === null) {
                conf = ansObj.confidence;
            }
            if (typeof conf === "number" && !Number.isNaN(conf)) {
                const thr = isMap ? resolveMinConfidence(ansObj, minConfidence) : minConfidence;
                if (conf < thr) {
                    ansObj.low_confidence = true;
                }
            }
        }
    }
}
export const TEMP_MIN = 0.5, TEMP_MAX = 5.0;
export function clampTemperature(t) {
    if (t === null || t === undefined || t === "" || typeof t === "boolean")
        return 1.0;
    const f = typeof t === "number" ? t : Number(t);
    if (!Number.isFinite(f))
        return 1.0;
    return Math.min(TEMP_MAX, Math.max(TEMP_MIN, f));
}
export function tempBucket(qtype, k) {
    const size = k <= 2 ? "2" : k <= 5 ? "3-5" : k <= 10 ? "6-10" : "11+";
    return `${["choice", "score", "noul"][qtype]}:${size}`;
}
/**
 * Validate a histogram-binning recalibration map.
 *
 * Keys are option-count bucket strings like "choice:2", "choice:3-5", "score:6-10", "noul:2".
 * Values are objects with an integer `bins >= 1` and a `values` array of length `bins`
 * where each number is in [0.0, 1.0].
 */
export function checkBinningMap(m) {
    if (!m || typeof m !== "object" || Array.isArray(m)) {
        throw new Error(`binning_map must be an object of bucket -> {bins, values}, got ${pyRepr(m)}`);
    }
    const out = {};
    for (const [name, entry] of Object.entries(m)) {
        if (!BUCKET_KEY.test(name)) {
            throw new Error(`binning_map key ${pyRepr(name)} is not a bucket like "choice:2" or "score:3-5"`);
        }
        if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
            throw new Error(`binning_map[${pyRepr(name)}] must be an object with "bins" and "values", got ${pyRepr(entry)}`);
        }
        const rec = entry;
        const nBins = rec.bins;
        const values = rec.values;
        if (typeof nBins === "boolean" || typeof nBins !== "number" || !Number.isInteger(nBins) || nBins < 1) {
            throw new Error(`binning_map[${pyRepr(name)}] must have an integer "bins" >= 1, got ${pyRepr(nBins)}`);
        }
        if (!Array.isArray(values) || values.length !== nBins) {
            throw new Error(`binning_map[${pyRepr(name)}] must have "values" of length "bins" (${nBins})`);
        }
        const parsedValues = [];
        for (const v of values) {
            if (typeof v === "boolean" || typeof v !== "number" || !Number.isFinite(v) || v < 0.0 || v > 1.0) {
                throw new Error(`binning_map[${pyRepr(name)}] values must be numbers in [0, 1], got ${pyRepr(v)}`);
            }
            parsedValues.push(v);
        }
        out[name] = { bins: nBins, values: parsedValues };
    }
    return out;
}
/**
 * Recalibrate one `answer_confidence` for its option-count `bucket` (tempBucket).
 *
 * Returns the confidence unchanged when the map has no entry for the bucket, so a bucket the map
 * was not fit for passes through rather than being forced to a wrong value.
 */
export function applyBinningMap(confidence, bucket, binningMap) {
    if (!Number.isFinite(confidence))
        return confidence;
    const entry = binningMap?.[bucket];
    if (!entry) {
        return confidence;
    }
    const bins = entry.bins;
    const b = Math.min(bins - 1, Math.max(0, Math.floor(confidence * bins)));
    return entry.values[b];
}
/** Max of a length list without spread (Math.max(...arr) throws RangeError past ~100k args). */
export function maxOf(values, fallback = 0) {
    let m = fallback;
    for (let i = 0; i < values.length; i++)
        if (values[i] > m)
            m = values[i];
    return m;
}
/** TS parity of py `collate_items(batch, pad_id)`: batch = list of groups. */
export function collateItems(batch, padId) {
    const items = (batch ?? []).flat();
    if (items.length === 0)
        return null;
    let L = 0;
    let K = 0;
    for (const it of items) {
        if (it.ids.length > L)
            L = it.ids.length;
        if (it.markers.length > K)
            K = it.markers.length;
    }
    const hasTarget = items.some((it) => "target" in it);
    const inputIds = items.map((it) => [...it.ids, ...Array(L - it.ids.length).fill(padId)]);
    const attentionMask = items.map((it) => [...Array(it.ids.length).fill(1), ...Array(L - it.ids.length).fill(0)]);
    const markerPos = items.map((it) => [...it.markers, ...Array(K - it.markers.length).fill(0)]);
    const markerMask = items.map((it) => [...it.markers.map(() => true), ...Array(K - it.markers.length).fill(false)]);
    const qtype = items.map((it) => it.qtype);
    const label = items.map((it) => (typeof it.label === "number" ? it.label : -1));
    const meta = items.map((it) => {
        const { ids: _ids, markers: _markers, target: _target, ...m } = it;
        return m;
    });
    const out = { inputIds, attentionMask, markerPos, markerMask, qtype, label, meta };
    if (hasTarget) {
        out.target = items.map((it, i) => {
            const t = Array.isArray(it.target) ? it.target : [];
            const k = it.markers.length;
            if (t.length > k) {
                // Same guard as py collate_items: the limit is this item's own marker count, not the
                // batch-wide K — a wider sibling row must not legitimise extra entries (#311).
                throw new Error(`collateItems: item ${i} has ${t.length} target entries but only ${k} marker positions; ` +
                    "a target needs one entry per option");
            }
            return [...t, ...Array(K - t.length).fill(0)];
        });
    }
    return out;
}
