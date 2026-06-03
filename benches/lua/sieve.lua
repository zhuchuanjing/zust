local n = tonumber(arg[1])
local is_prime = {}
for i = 0, n do is_prime[i] = true end
if n >= 0 then is_prime[0] = false end
if n >= 1 then is_prime[1] = false end
local count = 0
for p = 2, n do
    if is_prime[p] then
        count = count + 1
        local step = p
        local j = p * p
        while j <= n do
            is_prime[j] = false
            j = j + step
        end
    end
end
print(count)
