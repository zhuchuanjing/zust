local n = tonumber(arg[1])
local m = {}
for i = 0, n - 1 do
    local key = tostring(i)
    m[key] = i
end
local sum = 0
for i = 0, n - 1 do
    local key = tostring(i)
    sum = sum + m[key]
end
print(sum)
