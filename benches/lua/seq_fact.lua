local n = tonumber(arg[1])
local result = 1
local m = 1000000007
for i = 1, n do
    result = (result * i) % m
end
print(result)
