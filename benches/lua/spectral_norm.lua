local function eval_A(i, j)
    return 1.0 / ((i + j) * (i + j + 1) / 2 + i + 1)
end

local function multiply_Av(n, v)
    local result = {}
    for i = 0, n - 1 do
        local sum = 0.0
        for j = 0, n - 1 do
            sum = sum + eval_A(i, j) * v[j + 1]
        end
        result[i + 1] = sum
    end
    return result
end

local function multiply_Atv(n, v)
    local result = {}
    for i = 0, n - 1 do
        local sum = 0.0
        for j = 0, n - 1 do
            sum = sum + eval_A(j, i) * v[j + 1]
        end
        result[i + 1] = sum
    end
    return result
end

local n = tonumber(arg[1])
local u = {}
local v = {}
for i = 0, n - 1 do u[i + 1] = 1.0 end
for _ = 1, 10 do
    v = multiply_Av(n, u)
    u = multiply_Atv(n, v)
end
local vBv = 0.0
local vv = 0.0
for i = 0, n - 1 do
    vBv = vBv + u[i + 1] * v[i + 1]
    vv = vv + v[i + 1] * v[i + 1]
end
local result = math.sqrt(vBv / vv)
print(math.floor(result * 1000000))
