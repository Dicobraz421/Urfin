-- Обзор Lua для дифференциального теста подсветки кода (ADR-0024 §8).
local M = {}
local x, y = 1, 2
local s = "строка \" с кавычкой"
local l = [[длинная
строка]]
local e = [==[ ещё ]] длиннее ]==]

--[[ длинный
комментарий ]]
--[==[ и такой
]] ]==]

local conf = {
  a = 1,
  b = "x",
  [1] = true,
  c = function(v)
    local r = v + 1
    return r
  end,
  nested = {
    1,
    2,
    3,
  },
}

function M.add(a, b)
  local c = a + b
  if c > 10 then
    print("много")
    print("очень")
  elseif c > 5 then
    print("средне")
  else
    print("мало")
    print("совсем")
  end
  return c
end

function M:method(...)
  for i = 1, 10 do
    local v = i * 2
    print(v)
  end
  for k, v in pairs(self) do
    print(k, v)
    print(v)
  end
  while x < 10 do
    x = x + 1
    y = y - 1
  end
  repeat
    x = x - 1
    y = y + 1
  until x < 0
  do
    local z = 1
    print(z)
  end
  return select("#", ...)
end

local function helper(cb)
  return cb(function(a)
    local b = a * 2
    local c = b + 1
    return c
  end)
end

local t = setmetatable({}, {
  __index = function(_, k)
    return k
  end,
})

goto done
print("пропущено")
::done::

return M
