// An app that signs people in with the official ASP.NET Core OpenID Connect middleware, against Stuntman.
// OIDC_AUTHORITY=http://localhost:8025/login/contoso-stg/v2.0 OIDC_CLIENT_ID=acme-app OIDC_CLIENT_SECRET=… dotnet run --urls http://127.0.0.1:5999
using System.Security.Claims;
using Microsoft.AspNetCore.Authentication;
using Microsoft.AspNetCore.Authentication.Cookies;
using Microsoft.AspNetCore.Authentication.OpenIdConnect;
using Microsoft.IdentityModel.Protocols.OpenIdConnect;

var b = WebApplication.CreateBuilder(args);
b.Services.AddAuthentication(o => { o.DefaultScheme = CookieAuthenticationDefaults.AuthenticationScheme; o.DefaultChallengeScheme = OpenIdConnectDefaults.AuthenticationScheme; })
    .AddCookie()
    .AddOpenIdConnect(o =>
    {
        o.Authority = Environment.GetEnvironmentVariable("OIDC_AUTHORITY");
        o.ClientId = Environment.GetEnvironmentVariable("OIDC_CLIENT_ID");
        o.ClientSecret = Environment.GetEnvironmentVariable("OIDC_CLIENT_SECRET");
        o.RequireHttpsMetadata = false;
        o.ResponseType = OpenIdConnectResponseType.Code;
        o.UsePkce = true;
        o.SaveTokens = true;
        o.MapInboundClaims = false;
        o.Scope.Add("offline_access");
        o.TokenValidationParameters.NameClaimType = "name";
        o.TokenValidationParameters.RoleClaimType = "roles";
    });
b.Services.AddAuthorization(o => o.AddPolicy("Finance", p => p.RequireClaim("groups", "Finance")));
var app = b.Build();
app.UseAuthentication();
app.UseAuthorization();
app.MapGet("/me", (ClaimsPrincipal u) => Results.Json(new { name = u.Identity?.Name, claims = u.Claims.Select(c => new { c.Type, c.Value }) })).RequireAuthorization();
app.MapGet("/approvals", () => "approvals: ok").RequireAuthorization("Finance");
app.MapGet("/token", async (HttpContext h) => Results.Json(new { refresh = await h.GetTokenAsync("refresh_token") is { Length: > 0 } })).RequireAuthorization();
app.Run();
