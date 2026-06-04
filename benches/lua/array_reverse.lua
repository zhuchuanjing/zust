local n = tonumber(arg[1])
local arr = {}
for i = 0, 999 do
    arr[i + 1] = i
end
local total = 0
for _ = 1, n do
    local half = 500
    for i = 0, half - 1 do
        local j = 999 - i + 1
        local tmp = arr[i + 1]
        arr[i + 1] = arr[j]
        arr[j] = tmp
    end
    for i = 0, 999 do
        total = total + arr[i + 1]
    end
end
print(total)
