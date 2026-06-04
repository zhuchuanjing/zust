local n = tonumber(arg[1])
local arr = {}
for i = 0, n - 1 do
    arr[i + 1] = i * 2
end
local sum = 0
for target = 0, n - 1 do
    local low, high = 0, n - 1
    local found = -1
    while low <= high do
        local mid = math.floor((low + high) / 2)
        if arr[mid + 1] == target * 2 then
            found = mid
            low = high + 1
        elseif arr[mid + 1] < target * 2 then
            low = mid + 1
        else
            high = mid - 1
        end
    end
    sum = sum + found
end
print(sum)
