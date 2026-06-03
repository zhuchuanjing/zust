local n = tonumber(arg[1])
local a, b = 0, 1
for _ = 1, n do
    a, b = b, (a + b) % 1000000007
end
print(a)
