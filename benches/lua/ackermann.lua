local function ack(m, n)
    if m == 0 then return n + 1 end
    if n == 0 then return ack(m - 1, 1) end
    return ack(m - 1, ack(m, n - 1))
end

local n = 6  -- default
if arg and arg[1] then n = tonumber(arg[1]) or 6 end
print(ack(3, n))
