local n = tonumber(arg[1])
local l = {}
for i = 0, n - 1 do l[i] = i end
local sum = 0
for _ = 1, 5 do
    for i = 0, n - 1 do sum = sum + l[i] end
end
print(sum)
