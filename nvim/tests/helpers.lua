local M = {}

local this = debug.getinfo(1, "S").source:sub(2)
M.tests_dir = vim.fn.fnamemodify(this, ":p:h")
M.nvim_dir = vim.fn.fnamemodify(M.tests_dir, ":h")

function M.prepend_runtimepath()
  vim.opt.runtimepath:prepend(M.nvim_dir)
end

function M.reload(mod)
  package.loaded[mod] = nil
  return require(mod)
end

function M.scratch_dir(name)
  local root = M.nvim_dir .. "/.tests-work"
  vim.fn.mkdir(root, "p")
  local uv = vim.uv or vim.loop
  local dir = string.format("%s/%s-%s", root, name, tostring(uv.hrtime()))
  vim.fn.mkdir(dir, "p")
  return dir
end

function M.executable(name, lines, dir_name)
  local dir = M.scratch_dir(dir_name or name)
  local path = dir .. "/" .. name
  vim.fn.writefile(lines, path)
  vim.fn.system({ "chmod", "+x", path })
  return path, dir
end

return M
