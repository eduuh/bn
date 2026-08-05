-- bn.exec — the one async entry point. Every feature shells out through here.
--
-- `json` runs `bn <args> --json`, unwraps the `{ ok, command, data, error }` envelope once,
-- and hands features the `data`. `run` is for side-effecting/text commands. `json_sync` is a
-- blocking variant reserved for the status component (which must return a string inline).

local M = {}

M.config = { cmd = nil }

--- Resolve the binary once: explicit override, else `bn` (live dispatcher), else the
--- transitional `bn-rs`. Falls back to "bn" so errors surface a sensible command.
local function bin()
  if M.config.cmd then
    return M.config.cmd
  end
  if vim.fn.executable("bn") == 1 then
    return "bn"
  end
  if vim.fn.executable("bn-rs") == 1 then
    return "bn-rs"
  end
  return "bn"
end

function M.setup(opts)
  M.config = vim.tbl_extend("force", M.config, opts or {})
end

local function notify_err(res)
  local msg = (res.stderr and res.stderr ~= "") and res.stderr or "command failed"
  vim.notify("bn: " .. vim.trim(msg), vim.log.levels.ERROR)
end

--- Async `bn <args> --json` → `cb(data)` on the main loop. `cb` is ALWAYS called — with the
--- envelope's `data` on success, or `nil` on any failure — so concurrent readers (the board)
--- never hang on one bad surface. Failures notify unless `opts.silent` (a missing surface on
--- an older binary shouldn't spam).
function M.json(args, cb, opts)
  opts = opts or {}
  local cmd = vim.list_extend({ bin() }, vim.deepcopy(args))
  table.insert(cmd, "--json")
  vim.system(cmd, { text = true }, function(res)
    vim.schedule(function()
      local function fail(msg)
        if not opts.silent then
          vim.notify("bn: " .. msg, vim.log.levels.ERROR)
        end
        cb(nil)
      end
      if res.code ~= 0 then
        return fail(vim.trim((res.stderr and res.stderr ~= "") and res.stderr or "command failed"))
      end
      local ok, decoded = pcall(vim.json.decode, res.stdout)
      if not ok or type(decoded) ~= "table" then
        return fail("could not parse JSON output")
      end
      if decoded.ok == false then
        return fail((decoded.error or {}).message or "error")
      end
      cb(decoded.data)
    end)
  end)
end

--- Async `bn <args> --json` → `cb(decoded)` with the WHOLE decoded object, not the
--- envelope's `data`. A few surfaces (`bn staff`, `bn staff feed`) emit a bare payload
--- (`{"agents":[…]}` / `{"cards":[…]}`) rather than the `{ ok, command, data }` envelope, so
--- `json` (which unwraps `.data`) would hand them `nil`. Same always-calls-back, `nil`-on-failure
--- contract as `json`, so the board never hangs on a missing or older surface.
function M.json_raw(args, cb, opts)
  opts = opts or {}
  local cmd = vim.list_extend({ bin() }, vim.deepcopy(args))
  table.insert(cmd, "--json")
  vim.system(cmd, { text = true }, function(res)
    vim.schedule(function()
      local function fail(msg)
        if not opts.silent then
          vim.notify("bn: " .. msg, vim.log.levels.ERROR)
        end
        cb(nil)
      end
      local function parse(stdout)
        local ok, decoded = pcall(vim.json.decode, stdout or "")
        if ok and type(decoded) == "table" then
          return decoded
        end
        return nil
      end
      local decoded
      if res.code ~= 0 then
        -- Some surfaces still emit a useful JSON payload alongside a non-zero exit — notably
        -- `bn staff plan` on a broken gather, whose JSON carries `skill` / `gather_error`. Opt in
        -- with `accept_nonzero_json` to parse it rather than throw it away.
        decoded = opts.accept_nonzero_json and parse(res.stdout) or nil
        if decoded == nil then
          return fail(vim.trim((res.stderr and res.stderr ~= "") and res.stderr or "command failed"))
        end
      else
        decoded = parse(res.stdout)
        if decoded == nil then
          return fail("could not parse JSON output")
        end
      end
      -- Tolerate an enveloped payload too (`ok:false` still fails; `data` unwrapped when present)
      -- so this reader keeps working if a surface later adopts the standard envelope.
      if decoded.ok == false then
        return fail((decoded.error or {}).message or "error")
      end
      if type(decoded.data) == "table" then
        return cb(decoded.data)
      end
      cb(decoded)
    end)
  end)
end

--- Async side-effecting `bn <args>` (no --json) → optional `cb(stdout)`.
function M.run(args, cb)
  local cmd = vim.list_extend({ bin() }, vim.deepcopy(args))
  vim.system(cmd, { text = true }, function(res)
    vim.schedule(function()
      if res.code ~= 0 then
        return notify_err(res)
      end
      if cb then
        cb(res.stdout or "")
      end
    end)
  end)
end

--- Blocking `bn <args> --json` → `data` or nil. Only for the status component.
function M.json_sync(args)
  local cmd = vim.list_extend({ bin() }, vim.deepcopy(args))
  table.insert(cmd, "--json")
  local out = vim.fn.system(cmd)
  if vim.v.shell_error ~= 0 then
    return nil
  end
  local ok, decoded = pcall(vim.json.decode, out)
  if not ok or type(decoded) ~= "table" or decoded.ok == false then
    return nil
  end
  return decoded.data
end

--- Blocking `bn <args> --json` → the WHOLE decoded object (bare or enveloped), or nil. The sync
--- twin of `json_raw`, used only by the cockpit's open-time probe that decides whether the
--- BRIEF/STAFF panels are present at all (they exist only when the staff framework is installed).
function M.json_sync_raw(args)
  local cmd = vim.list_extend({ bin() }, vim.deepcopy(args))
  table.insert(cmd, "--json")
  local out = vim.fn.system(cmd)
  if vim.v.shell_error ~= 0 then
    return nil
  end
  local ok, decoded = pcall(vim.json.decode, out)
  if not ok or type(decoded) ~= "table" or decoded.ok == false then
    return nil
  end
  if type(decoded.data) == "table" then
    return decoded.data
  end
  return decoded
end

return M
