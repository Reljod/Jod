#!/usr/bin/env node
/**
 * orc — orchestrate Claude Code background sessions as an agent team.
 *
 * Every session started here is a plain `claude --bg` session, so it shows up
 * in `claude agents` / the agent view exactly like one you started by hand.
 * There is no private channel and no daemon of its own — this only composes
 * primitives the CLI already ships:
 *
 *   claude --bg "<task>"           start a session          -> orc spawn
 *   claude --bg -r <sid> "<msg>"   continue one, in context -> orc send
 *   claude agents --json --all     enumerate them           -> orc ls
 *   claude logs|stop <id>          inspect / kill           -> orc logs|stop
 *
 * Results are harvested from ~/.claude/jobs/<shortId>/state.json, which the
 * CLI maintains: { state, detail, output: { result }, sessionId, cwd, ... }.
 */

import { execFile, execFileSync } from 'node:child_process'
import { readFileSync, writeFileSync, existsSync, readdirSync, copyFileSync, statSync } from 'node:fs'
import { homedir } from 'node:os'
import { join, resolve as resolvePath, basename } from 'node:path'

const HOME = homedir()
const JOBS = join(HOME, '.claude', 'jobs')
const CONFIG = join(HOME, '.claude.json')

const CLAUDE_BIN =
  process.env.CLAUDE_BIN ||
  [
    join(HOME, '.claude', 'local', 'node_modules', '@anthropic-ai', 'claude-code', 'bin', 'claude.exe'),
    '/usr/local/bin/claude',
    '/opt/homebrew/bin/claude',
  ].find(existsSync) ||
  'claude'

/** A session is finished; `blocked` means it stopped to ask the user something. */
const TERMINAL = new Set(['done', 'error', 'stopped'])

// ---------------------------------------------------------------- utilities

const die = (msg) => {
  console.error(`orc: ${msg}`)
  process.exit(1)
}

const readJson = (path, fallback = null) => {
  try {
    return JSON.parse(readFileSync(path, 'utf8'))
  } catch {
    return fallback
  }
}

const stripAnsi = (s) => s.replace(/\x1b\[[0-9;]*[a-zA-Z]/g, '')

const claude = (args) => execFileSync(CLAUDE_BIN, args, { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 })

const claudeIn = (args, cwd) =>
  new Promise((ok, fail) => {
    execFile(CLAUDE_BIN, args, { cwd, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 }, (err, stdout, stderr) =>
      err ? fail(new Error(stripAnsi(stderr || err.message))) : ok(stripAnsi(stdout)),
    )
  })

/** `claude --bg` announces "backgrounded · <shortId>" and returns immediately. */
const parseId = (out) => {
  const id = stripAnsi(out).match(/backgrounded\s+·\s+(\w+)/)?.[1]
  if (!id) die(`could not find a session id in claude's output:\n${out}`)
  return id
}

const jobState = (shortId) => readJson(join(JOBS, shortId, 'state.json'))

/** Job dirs on disk, newest first — `claude agents` drops reaped sessions. */
const allJobs = () =>
  existsSync(JOBS)
    ? readdirSync(JOBS)
        .filter((d) => existsSync(join(JOBS, d, 'state.json')))
        .map((id) => ({ id, ...jobState(id) }))
        .sort((a, b) => String(b.updatedAt ?? '').localeCompare(String(a.updatedAt ?? '')))
    : []

const liveSessions = () => {
  try {
    return JSON.parse(claude(['agents', '--json', '--all']))
  } catch {
    return []
  }
}

// ------------------------------------------------------- project references

const projectEntries = () => Object.entries(readJson(CONFIG, {})?.projects ?? {})

/**
 * `@Socially` -> the project directory named Socially.
 *
 * Candidates come from the directories Claude already knows about (the
 * `projects` map in ~/.claude.json), so `@name` works from anywhere without
 * configuring a workspace root. A plain path is returned as-is.
 */
const resolveProject = (ref) => {
  if (!ref) die('missing project reference')
  if (!ref.startsWith('@')) return resolvePath(ref)

  const name = ref.slice(1)
  if (!name) die('empty project reference: "@"')

  const dirs = projectEntries().map(([path]) => path)
  const exact = dirs.filter((d) => basename(d).toLowerCase() === name.toLowerCase())
  const fuzzy = exact.length ? exact : dirs.filter((d) => basename(d).toLowerCase().includes(name.toLowerCase()))

  const existing = fuzzy.filter((d) => existsSync(d) && statSync(d).isDirectory())
  if (!existing.length) {
    const known = dirs.filter(existsSync).map((d) => `  @${basename(d)}`).sort().join('\n')
    die(`no project matching "${ref}".\nKnown projects:\n${known || '  (none)'}`)
  }
  if (existing.length > 1) {
    // Same basename in two checkouts is common (a dev clone and an installed
    // copy). Never guess which one — show trust state and make them pick.
    const rows = existing.map((d) => `  ${isTrusted(d) ? '  ' : '! '}${d}`).join('\n')
    die(`"${ref}" is ambiguous — pass the full path:\n${rows}\n\n"!" = not trusted.`)
  }
  return existing[0]
}

/**
 * A directory Claude has never been trusted in stops the new session on an
 * interactive trust/MCP prompt: the job starts, shows up in the agent view,
 * and silently never reaches the model. Refuse up front instead.
 */
const isTrusted = (dir) => readJson(CONFIG, {})?.projects?.[dir]?.hasTrustDialogAccepted === true

const assertSpawnable = (dir) => {
  if (!existsSync(dir)) die(`no such directory: ${dir}`)
  if (isTrusted(dir)) return
  die(
    `${dir} is not a trusted project directory.\n` +
      `A session spawned there would hang on the interactive trust/MCP prompt.\n` +
      `Fix: \`jod orc trust ${dir}\`, or open Claude in that directory once.`,
  )
}

// ------------------------------------------------------- session references

/** Accept a short job id, a full session UUID, or a (partial) session name. */
const resolveSession = (ref) => {
  if (!ref) die('missing session reference')

  const direct = jobState(ref)
  if (direct) return { shortId: ref, ...direct }

  const jobs = allJobs()
  const bySid = jobs.find((j) => j.sessionId === ref)
  if (bySid) return { shortId: bySid.id, ...bySid }

  const needle = ref.toLowerCase()
  const byName = jobs.filter((j) => String(j.name ?? '').toLowerCase().includes(needle))
  if (byName.length === 1) return { shortId: byName[0].id, ...byName[0] }
  if (byName.length > 1) {
    die(`"${ref}" matches several sessions:\n${byName.map((j) => `  ${j.id}  ${j.name}`).join('\n')}`)
  }

  const live = liveSessions().find((s) => s.id === ref || s.sessionId === ref || s.name === ref)
  if (live) return { shortId: live.id, ...live }

  die(`no session matching "${ref}"`)
}

// ---------------------------------------------------------------- commands

const cmdList = (args) => {
  const rows = (args.includes('--live') ? liveSessions().map((s) => ({ id: s.id ?? '-', ...s })) : allJobs()).map(
    (j) => ({ id: j.id, name: j.name ?? '', state: j.state ?? j.kind ?? '?', cwd: j.cwd ?? '' }),
  )

  if (args.includes('--json')) return console.log(JSON.stringify(rows, null, 2))
  if (!rows.length) return console.log('(no sessions)')

  const nameWidth = Math.max(...rows.map((r) => r.name.length), 4)
  for (const r of rows) {
    console.log(
      `${r.id.padEnd(10)} ${r.state.padEnd(8)} ${r.name.padEnd(nameWidth)}  ${r.cwd.replace(`${HOME}/`, '~/')}`,
    )
  }
}

const cmdSpawn = async (args) => {
  const [target, ...rest] = args
  const task = rest.join(' ').trim()
  if (!target || !task) die('usage: jod orc spawn <@project|dir> <task...>')

  const cwd = resolveProject(target)
  assertSpawnable(cwd)
  console.log(parseId(await claudeIn(['--bg', task], cwd)))
}

const cmdSend = async (args) => {
  const [ref, ...rest] = args
  const message = rest.join(' ').trim()
  if (!ref || !message) die('usage: jod orc send <id|name> <message...>')

  const s = resolveSession(ref)
  if (!s.sessionId) die(`session ${s.shortId} has no sessionId yet — it is probably still starting`)
  if (!TERMINAL.has(s.state) && s.state !== 'blocked') {
    console.error(
      `orc: warning — ${s.shortId} is "${s.state}"; resuming a busy session runs a second process against its transcript`,
    )
  }
  console.log(parseId(await claudeIn(['--bg', '-r', s.sessionId, message], s.cwd ?? process.cwd())))
}

/**
 * Fan one task out across several projects, or several tasks across one.
 *   jod orc fanout @Jod @Socially -- "audit the README"
 *   jod orc fanout --spec team.json      [{ "project": "@Jod", "task": "..." }]
 */
const cmdFanout = async (args) => {
  let spec

  if (args[0] === '--spec') {
    const src = args[1]
    if (!src) die('usage: jod orc fanout --spec <file.json|->')
    const raw = src === '-' ? readFileSync(0, 'utf8') : readFileSync(src, 'utf8')
    try {
      spec = JSON.parse(raw)
    } catch (e) {
      die(`spec is not valid JSON: ${e.message}`)
    }
    if (!Array.isArray(spec) || !spec.length) die('spec must be a non-empty array of { project, task }')
  } else {
    const sep = args.indexOf('--')
    if (sep < 1) die('usage: jod orc fanout <@project...> -- <task...>')
    const task = args.slice(sep + 1).join(' ').trim()
    if (!task) die('fanout needs a task after "--"')
    spec = args.slice(0, sep).map((project) => ({ project, task }))
  }

  // Resolve and validate every target before starting any of them, so a typo
  // in the last entry doesn't leave half a team running.
  const jobs = spec.map((t, i) => {
    if (!t?.project || !t?.task) die(`spec[${i}] needs both "project" and "task"`)
    const cwd = resolveProject(t.project)
    assertSpawnable(cwd)
    return { cwd, task: t.task }
  })

  const ids = await Promise.all(jobs.map((j) => claudeIn(['--bg', j.task], j.cwd).then(parseId)))
  ids.forEach((id, i) => console.log(`${id}\t${jobs[i].cwd.replace(`${HOME}/`, '~/')}`))
}

const cmdResult = (args) => {
  const s = resolveSession(args[0])
  const st = jobState(s.shortId) ?? {}
  const result = st.output?.result

  if (result === undefined) {
    console.error(`orc: ${s.shortId} is "${st.state}" with no result yet`)
    if (st.detail) console.error(`     ${st.detail}`)
    if (st.needs) console.error(`     needs: ${st.needs}`)
    process.exit(2)
  }
  console.log(typeof result === 'string' ? result : JSON.stringify(result, null, 2))
}

/** Block until every named session finishes or stops to ask a question. */
const cmdWait = async (args) => {
  const timeout = Number(args.find((a) => a.startsWith('--timeout='))?.split('=')[1] ?? 900) * 1000
  const refs = args.filter((a) => !a.startsWith('--'))
  if (!refs.length) die('usage: jod orc wait <id|name>... [--timeout=<seconds>]')

  const ids = refs.map((r) => resolveSession(r).shortId)
  const settled = (s) => TERMINAL.has(s) || s === 'blocked'
  const deadline = Date.now() + timeout

  while (Date.now() < deadline) {
    const states = ids.map((id) => [id, jobState(id)?.state ?? 'unknown'])
    if (states.every(([, s]) => settled(s))) {
      for (const [id, s] of states) console.log(`${id}\t${s}`)
      return
    }
    await new Promise((r) => setTimeout(r, 3000))
  }
  die(`timed out after ${timeout / 1000}s`)
}

const cmdLogs = (args) => process.stdout.write(stripAnsi(claude(['logs', resolveSession(args[0]).shortId])))

const cmdStop = (args) => process.stdout.write(stripAnsi(claude(['stop', resolveSession(args[0]).shortId])))

/** Explicit, backed-up edit to ~/.claude.json so spawns there don't hang. */
const cmdTrust = (args) => {
  const dir = resolveProject(args[0])
  if (!existsSync(dir)) die(`no such directory: ${dir}`)
  if (isTrusted(dir)) return console.log(`already trusted: ${dir}`)

  const cfg = readJson(CONFIG)
  if (!cfg) die(`cannot read ${CONFIG}`)
  copyFileSync(CONFIG, `${CONFIG}.orc-backup`)

  const prev = cfg.projects?.[dir] ?? {}
  cfg.projects = {
    ...(cfg.projects ?? {}),
    [dir]: {
      ...prev,
      hasTrustDialogAccepted: true,
      enabledMcpjsonServers: prev.enabledMcpjsonServers ?? [],
      disabledMcpjsonServers: prev.disabledMcpjsonServers ?? [],
    },
  }
  writeFileSync(CONFIG, JSON.stringify(cfg, null, 2))
  console.log(`trusted: ${dir}\n(backup: ${CONFIG}.orc-backup)`)
}

const cmdProjects = () => {
  const dirs = projectEntries()
    .filter(([d]) => existsSync(d))
    .map(([d, v]) => `${v?.hasTrustDialogAccepted ? '  ' : '! '}@${basename(d)}`.padEnd(28) + d)
    .sort()
  console.log(dirs.join('\n') || '(none)')
  console.log('\n"!" = not trusted; `jod orc trust @name` before spawning there.')
}

const usage = `jod orc — run Claude Code background sessions as an agent team

  jod orc ls [--live] [--json]           list sessions
  jod orc projects                       list @project references
  jod orc spawn <@project|dir> <task>    start a session, print its id
  jod orc send <id|name> <message>       continue a session, in context
  jod orc fanout <@a> <@b> -- <task>     same task across projects
  jod orc fanout --spec <file.json>      [{ "project": "@Jod", "task": "..." }]
  jod orc result <id|name>               its final result
  jod orc wait <id|name>... [--timeout=S]  until finished or blocked
  jod orc logs <id|name>                 terminal output
  jod orc stop <id|name>                 stop it
  jod orc trust <@project|dir>           make a directory spawn-safe

Sessions appear in \`claude agents\` and the agent view like any other.`

const COMMANDS = {
  ls: cmdList,
  list: cmdList,
  projects: cmdProjects,
  spawn: cmdSpawn,
  send: cmdSend,
  fanout: cmdFanout,
  result: cmdResult,
  wait: cmdWait,
  logs: cmdLogs,
  stop: cmdStop,
  trust: cmdTrust,
}

const [cmd, ...rest] = process.argv.slice(2)
if (!cmd || ['-h', '--help', 'help'].includes(cmd)) {
  console.log(usage)
  process.exit(0)
}

const handler = COMMANDS[cmd]
if (!handler) die(`unknown command "${cmd}"\n\n${usage}`)

try {
  await handler(rest)
} catch (e) {
  die(e.message)
}
