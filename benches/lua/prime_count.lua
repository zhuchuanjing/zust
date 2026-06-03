local function is_prime(n)
    if n < 2 then return false end
    local i = 2
    while i * i <= n do
        if n % i == 0 then return false end
        i = i + 1
    end
    return true
end

local n = tonumber(arg[1])
local count = 0
for x = 2, n do
    if is_prime(x) then count = count + 1 end
end
print(count)
