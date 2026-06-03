local n = tonumber(arg[1])
local total = 0
for start = 1, n do
    local x = start
    while x ~= 1 do
        if x % 2 == 0 then
            x = x / 2
        else
            x = 3 * x + 1
        end
        total = total + 1
    end
end
print(total)
