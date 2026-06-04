local n = tonumber(arg[1])
local total = 0
for y = 0, n - 1 do
    for x = 0, n - 1 do
        local cr = 1.5 * (x - n / 2) / (0.5 * n)
        local ci = (y - n / 2) / (0.5 * n)
        local zr, zi = 0.0, 0.0
        local k = 0
        while k < 50 and zr * zr + zi * zi < 4 do
            zr, zi = zr * zr - zi * zi + cr, 2 * zr * zi + ci
            k = k + 1
        end
        total = total + k
    end
end
print(total)
