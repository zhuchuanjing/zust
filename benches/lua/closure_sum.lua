local n = tonumber(arg[1])
local acc = 0
local function add(a, b)
    return a + b
end
for i = 0, n - 1 do
    acc = add(acc, i)
end
print(acc)
