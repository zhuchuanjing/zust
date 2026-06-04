local n = tonumber(arg[1])
local seed = 12345
local total = 0
for _ = 1, n do
    seed = seed * 1103515245 + 12345
    seed = seed & 0x7fffffff
    total = total + (seed & 0xff)
end
print(total)
