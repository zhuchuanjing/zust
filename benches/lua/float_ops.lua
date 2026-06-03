local n = tonumber(arg[1])
local x = 1.0
local y = 2.0
for _ = 1, n do
    x = x * 1.000001 + y * 0.999999
    y = y * 1.000001 - x * 0.999999
end
print(math.floor(x + y))
