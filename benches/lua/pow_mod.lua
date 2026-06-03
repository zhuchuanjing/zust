local n = tonumber(arg[1])
local result = 1
for i = 0, n - 1 do
    local base = (i % 100) + 2
    local exp = (i % 31) + 1
    local m = 1000000007
    local r = 1
    local b = base
    local e = exp
    while e > 0 do
        if e % 2 == 1 then
            r = (r * b) % m
        end
        b = (b * b) % m
        e = math.floor(e / 2)
    end
    result = (result + r) % m
end
print(result)
