local function gcd(a, b)
    while b ~= 0 do
        a, b = b, a % b
    end
    return a
end

local n = tonumber(arg[1])
local total = 0
for i = 0, n - 1 do
    total = total + gcd(i, n - i)
end
print(total)
