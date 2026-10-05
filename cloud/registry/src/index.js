// The shared question registry. Janus installs whose owners opted in send interview questions,
// already rewritten on their Mac to name no person, company or product. They wait as "pending"
// until the maintainer approves them from their own Mac with wrangler (there is no admin route
// here). Every Janus reads the approved set for practice interviews.
//
//   GET    /v1/questions        the approved questions
//   POST   /v1/contributions    {install, app_version, questions: [{text, kind, round?, role?, company?}]}
//   DELETE /v1/contributions    {install}: withdraw everything that install sent

const KINDS = new Set(["intro", "behavioral", "technical", "situational", "motivation", "role_specific", "other"]);
const INSTALL = /^[0-9a-f]{32}$/;
const MAX_PER_REQUEST = 20;
const MIN_TEXT = 12;
const MAX_TEXT = 400;
const MAX_FIELD = 80;
const MAX_BODY = 32 * 1024;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    try {
      if (url.pathname === "/v1/questions" && request.method === "GET") return await questions(env);
      if (url.pathname === "/v1/contributions" && request.method === "POST") return await contribute(request, env);
      if (url.pathname === "/v1/contributions" && request.method === "DELETE") return await withdraw(request, env);
      return json({ error: "not found" }, 404);
    } catch (error) {
      if (error instanceof BadRequest) return json({ error: error.message }, 400);
      console.error(error);
      return json({ error: "something went wrong" }, 500);
    }
  },
};

class BadRequest extends Error {}

function json(value, status = 200, headers = {}) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json; charset=utf-8", ...headers },
  });
}

async function body(request) {
  const text = await request.text();
  if (text.length > MAX_BODY) throw new BadRequest("too large");
  try {
    return JSON.parse(text);
  } catch {
    throw new BadRequest("not JSON");
  }
}

function field(value) {
  if (value === undefined || value === null || value === "") return null;
  if (typeof value !== "string" || value.length > MAX_FIELD) throw new BadRequest("a field is too long");
  return value.trim();
}

async function questions(env) {
  const { results } = await env.DB.prepare(
    "SELECT id, text, kind, rounds, roles, contributors FROM questions ORDER BY contributors DESC, id LIMIT 5000",
  ).all();
  const list = results.map((q) => ({ ...q, rounds: JSON.parse(q.rounds), roles: JSON.parse(q.roles) }));
  return json({ version: 1, questions: list }, 200, { "cache-control": "public, max-age=3600" });
}

async function contribute(request, env) {
  const input = await body(request);
  if (!INSTALL.test(input.install ?? "")) throw new BadRequest("install must be 32 hex digits");
  const items = input.questions;
  if (!Array.isArray(items) || items.length === 0 || items.length > MAX_PER_REQUEST) {
    throw new BadRequest(`questions must be a list of 1 to ${MAX_PER_REQUEST}`);
  }
  const rows = items.map((q) => {
    const text = typeof q?.text === "string" ? q.text.trim().replace(/\s+/g, " ") : "";
    if (text.length < MIN_TEXT || text.length > MAX_TEXT) throw new BadRequest(`each question is ${MIN_TEXT}-${MAX_TEXT} characters`);
    if (!KINDS.has(q.kind)) throw new BadRequest("unknown kind");
    return [text, q.kind, field(q.round), field(q.role), field(q.company)];
  });
  const today = await env.DB.prepare(
    "SELECT COUNT(*) AS everyone, SUM(install = ?1) AS mine FROM contributions WHERE created_at >= datetime('now', 'start of day')",
  ).bind(input.install).first();
  if ((today.everyone ?? 0) + rows.length > Number(env.DAILY_CAP)) {
    return json({ error: "the registry is full for today; try again tomorrow" }, 429, { "retry-after": "3600" });
  }
  if ((today.mine ?? 0) + rows.length > Number(env.INSTALL_DAILY_CAP)) {
    return json({ error: "too many from this install today" }, 429, { "retry-after": "3600" });
  }
  const version = field(input.app_version);
  const insert = env.DB.prepare(
    "INSERT OR IGNORE INTO contributions (install, text, kind, round, role, company, app_version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
  );
  const results = await env.DB.batch(rows.map((r) => insert.bind(input.install, ...r, version)));
  const accepted = results.reduce((n, r) => n + (r.meta?.changes ?? 0), 0);
  return json({ accepted, duplicates: rows.length - accepted });
}

async function withdraw(request, env) {
  const input = await body(request);
  if (!INSTALL.test(input.install ?? "")) throw new BadRequest("install must be 32 hex digits");
  const result = await env.DB.prepare("DELETE FROM contributions WHERE install = ?1").bind(input.install).run();
  return json({ withdrawn: result.meta?.changes ?? 0 });
}
