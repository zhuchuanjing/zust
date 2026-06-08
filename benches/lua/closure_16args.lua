local n = tonumber(arg[1])
local acc = 0
local function sum16(a, b, c, d, e, f, g, h, i, j, k, l, m, n_arg, o, p)
    return a + b + c + d + e + f + g + h + i + j + k + l + m + n_arg + o + p
end
for idx = 0, n - 1 do
    acc = acc + sum16(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16)
end
print(acc)
