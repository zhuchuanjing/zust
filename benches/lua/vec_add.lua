local n = tonumber(arg[1])
local size = 100
local a = {}
for i = 0, size - 1 do
    a[i + 1] = i * 1.5
end
local total = 0.0
for _ = 0, n - 1 do
    for i = 0, size - 1 do
        total = total + a[i + 1]
    end
end
print(math.floor(total))
