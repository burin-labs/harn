local M = {}

function M.greet(name)
    return "hello " .. name
end

function M:shout(message)
    return message:upper()
end

local function helper(x)
    return x + 1
end

return M
