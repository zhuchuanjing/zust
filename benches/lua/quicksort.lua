local function partition(arr, low, high)
    local pivot = arr[high + 1]
    local i = low
    for j = low, high - 1 do
        if arr[j + 1] <= pivot then
            arr[i + 1], arr[j + 1] = arr[j + 1], arr[i + 1]
            i = i + 1
        end
    end
    arr[i + 1], arr[high + 1] = arr[high + 1], arr[i + 1]
    return i
end

local function quicksort(arr, low, high)
    if low < high then
        local pi = partition(arr, low, high)
        quicksort(arr, low, pi - 1)
        quicksort(arr, pi + 1, high)
    end
end

local n = tonumber(arg[1])
local arr = {}
for i = 0, n - 1 do
    arr[i + 1] = i * 6364136223846793005 + 1
end
quicksort(arr, 0, n - 1)
local sum = 0
for i = 0, n - 1 do
    sum = sum + arr[i + 1]
end
print(sum)
