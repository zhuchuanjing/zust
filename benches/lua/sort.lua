local function bubble_sort(items)
    local n = #items
    for i = 1, n do
        for j = 1, n - i do
            if items[j] > items[j + 1] then
                items[j], items[j + 1] = items[j + 1], items[j]
            end
        end
    end
end

local n = tonumber(arg[1])
local items = {}
for i = 0, n - 1 do
    local seed = i * 6364136223846793005 + 1
    items[i + 1] = seed
end
bubble_sort(items)
local sum = 0
for i = 1, n do sum = sum + items[i] end
print(sum)
