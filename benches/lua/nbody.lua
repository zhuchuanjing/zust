local n = tonumber(arg[1])
local bodies = 100
local total = 0
for step = 1, n do
    for i = 0, bodies - 1 do
        for j = 0, bodies - 1 do
            if i ~= j then
                total = total + i * j
            end
        end
    end
end
print(total)
