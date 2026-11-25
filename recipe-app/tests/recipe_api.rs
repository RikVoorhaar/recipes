#[allow(dead_code)]
mod support;

use serde_json::{json, Value};
use std::io::Cursor;

fn bearer(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    request.bearer_auth("test-secret")
}

async fn create(app: &support::TestApp) -> Value {
    bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
        .json(&json!({
            "title": "Butter noodles",
            "description": "A simple supper",
            "servings": 2,
            "tags": ["quick"],
            "body": "@butter{20%g}\n@pasta{200%g}"
        }))
        .send()
        .await
        .expect("create request")
        .json()
        .await
        .expect("create response")
}

#[tokio::test]
async fn public_pages_scaled_view_and_validation_are_read_only() {
    let app = support::TestApp::start().await;
    let created = create(&app).await;
    let recipe = &created["recipe"];
    let id = recipe["id"].as_str().unwrap();
    let before = app.revision_count().await;
    let ingredient_count = app.ingredient_count().await;

    let index = app.get("/").await;
    assert_eq!(index.status(), 200);
    let index = index.text().await.unwrap();
    assert!(index.contains("<title>Recipes</title>"));
    assert!(!index.contains("wordmark"));
    assert!(index.contains("Butter noodles"));
    assert!(index.contains("No picture"));
    assert!(index.contains("<div class=\"card-image\"><div class=\"placeholder\""));
    assert!(index.contains("<div class=\"card-copy\"><h2 class=\"card-title\">"));
    assert!(!index.contains("card-kicker"));
    assert!(!index.contains("Hearth &amp; Table"));
    assert!(index.contains("href=\"/assets/site.css\""));
    assert!(!index.contains("<style"));
    assert!(index.contains("href=\"/ingredients\""));
    let css = app.get("/assets/site.css").await;
    assert_eq!(css.status(), 200);
    assert!(css.headers()[reqwest::header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/css"));
    let css = css.text().await.unwrap();
    assert!(css.contains(":focus-visible"));
    assert!(css.contains(".recipe-card:focus-within"));
    assert!(css.contains(".recipe-card .tag:focus-visible"));
    assert!(css.contains("outline: 3px solid #9a4f26"));
    assert!(css.contains("overflow: hidden"));
    assert!(css.contains(
        ".recipe-grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr));"
    ));
    assert!(css.contains(".recipe-grid { grid-template-columns: 1fr;"));
    assert!(css.contains("--color-paper"));
    let font = app.get("/assets/Lora.ttf").await;
    assert_eq!(font.status(), 200);
    assert!(font.headers()[reqwest::header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("font/ttf"));
    let detail = app.get(&format!("/recipes/{id}?servings=4")).await;
    assert_eq!(detail.status(), 200);
    let detail = detail.text().await.unwrap();
    assert!(detail.contains("name=\"viewport\""));
    assert!(detail.contains("<title>Butter noodles</title>"));
    assert_eq!(detail.matches(">Recipes</a>").count(), 1);
    assert!(!detail.contains("<script"));
    assert!(detail.contains(">Printable PDF</a>"));
    assert!(detail.contains("value=\"4\""));
    assert!(detail.contains("400 g"));
    assert!(detail.contains("action=\"/recipes/"));
    assert!(detail.contains("id=\"servings\""));
    assert_eq!(app.get("/tags/quick").await.status(), 200);
    let scaled: Value = bearer(
        reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}/scaled?servings=4"))),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(scaled["servings"], 4);
    assert_eq!(scaled["ingredients"][1]["quantity"], "400 g");

    let dry_run: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes/validate")))
        .json(&json!({
            "title": "Preview",
            "servings": 2,
            "target_servings": 4,
            "body": "@butter{20%g}\n@pasta{200%g}\n@unregistered"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        dry_run["resolutions"].as_array().unwrap().last().unwrap()["would_create"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(dry_run["print_fit"]["status"], "fits");
    assert_eq!(dry_run["print_fit"]["font_size_pt"], 11);
    let after = app.revision_count().await;
    assert_eq!(before, after);
    assert_eq!(ingredient_count, app.ingredient_count().await);
    let public_index = app.get("/").await.text().await.unwrap();
    assert!(!public_index.contains("revision"));
}

#[tokio::test]
async fn ingredient_prologue_does_not_duplicate_rows_or_render_as_a_method_step() {
    let app = support::TestApp::start().await;
    let body = "@olive oil{15%ml}\n@onion{1}, diced\n@carrot{2}, sliced\n@celery stalk{2}, sliced\n@garlic clove{2}, minced\n@chicken thigh{600%g}, bone-in\n@chicken stock{1.5%l}\n@bay leaf{1}\n@thyme{2%sprig}\n@salt{1%tsp}\n@black pepper{0.5%tsp}\n@egg noodle{150%g}\n@parsley{10%g}, chopped\n\nHeat @olive oil{} in a large pot over medium heat. Add @onion{}, @carrot{}, and @celery stalk{}; cook until softened. Stir in @garlic clove{} briefly.\n\nAdd @chicken thigh{}, @chicken stock{}, @bay leaf{}, @thyme{}, @salt{}, and @black pepper{}. Bring to a boil, then reduce to a gentle simmer. Cover and cook until the chicken is tender and reaches 74°C at its thickest point.\n\nRemove the chicken. Add @egg noodle{} to the pot and cook until tender according to the package directions. Discard the chicken skin and bones, shred the meat, and return it to the pot. Remove the bay leaf and thyme stems, stir in @parsley{}, and adjust seasoning.";
    let created: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
        .json(&json!({
            "title": "Chicken Soup",
            "description": "A comforting chicken soup with vegetables, noodles, and fresh parsley.",
            "servings": 4,
            "tags": ["soup", "chicken", "comfort food"],
            "body": body
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["recipe"]["id"].as_str().unwrap();
    let scaled: Value = bearer(
        reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}/scaled?servings=8"))),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let ingredients = scaled["ingredients"].as_array().unwrap();
    assert_eq!(
        ingredients.len(),
        13,
        "prologue quantities must not be duplicated"
    );
    assert_eq!(ingredients[0]["name"], "olive oil");
    assert_eq!(ingredients[0]["quantity"], "30 ml");
    assert_eq!(ingredients[1]["note"], "diced");
    assert_eq!(ingredients[5]["name"], "chicken thigh");
    assert_eq!(ingredients[5]["quantity"], "1 2/10 kg");
    assert_eq!(ingredients[5]["note"], "bone-in");
    let instructions = scaled["instructions"].as_array().unwrap();
    assert_eq!(
        instructions.len(),
        3,
        "the declaration prologue is not a method step"
    );
    assert!(instructions[0]
        .as_str()
        .unwrap()
        .starts_with("Heat olive oil in a large pot"));

    let html = app
        .get(&format!("/recipes/{id}?servings=8"))
        .await
        .text()
        .await
        .unwrap();
    assert!(html.contains("Heat <span class=\"ingredient-name\">olive oil</span> in a large pot"));
    assert!(html.contains("<span class=\"ingredient-name\">onion</span> — diced"));
    assert!(!html.contains("olive oil onion, diced carrot, sliced"));

    let pdf = app
        .get(&format!("/recipes/{id}.pdf?servings=8"))
        .await
        .bytes()
        .await
        .unwrap();
    let document = lopdf::Document::load_mem(&pdf).unwrap();
    assert_eq!(document.get_pages().len(), 1);
    let pdf_text = pdf_extract::extract_text_from_mem(&pdf)
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(pdf_text.contains("Heat olive oil in a large pot"));
    assert!(pdf_text.contains("diced"));
    assert!(!pdf_text.contains("olive oil onion, diced carrot, sliced"));
}

#[tokio::test]
async fn method_highlights_cooklang_ingredients_but_not_plain_words_or_cookware() {
    let app = support::TestApp::start().await;
    let created: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
        .json(&json!({
            "title": "Seasoning",
            "servings": 2,
            "body": "Stir @salt{2%g} into the #pan{}, then taste the salt & adjust."
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["recipe"]["id"].as_str().unwrap();
    let html = app
        .get(&format!("/recipes/{id}?servings=4"))
        .await
        .text()
        .await
        .unwrap();
    let salt = "<span class=\"ingredient-name\">salt</span>";
    assert!(html.contains(&format!("<li>4 g {salt}</li>")));
    assert!(html.contains(&format!(
        "<li>Stir {salt} into the pan, then taste the salt &amp; adjust.</li>"
    )));
    assert_eq!(html.matches(salt).count(), 2);
}

#[tokio::test]
async fn ingredient_only_recipes_and_distinct_measured_occurrences_are_preserved() {
    let app = support::TestApp::start().await;
    let create = |title: &str, body: &str| {
        bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
            .json(&json!({"title": title, "servings": 2, "body": body}))
    };

    let ingredient_only: Value = create(
        "Ingredient-only sauce",
        "@butter{20%g}\n@flour{100%g}\n@milk{200%ml}",
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let id = ingredient_only["recipe"]["id"].as_str().unwrap();
    let scaled: Value =
        bearer(reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}/scaled"))))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(scaled["ingredients"].as_array().unwrap().len(), 3);
    assert!(scaled["instructions"].as_array().unwrap().is_empty());

    let unspecified: Value = create("Unquantified ingredients", "@salt\n@pepper")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = unspecified["recipe"]["id"].as_str().unwrap();
    let scaled: Value =
        bearer(reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}/scaled"))))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(scaled["ingredients"].as_array().unwrap().len(), 2);
    assert!(scaled["instructions"].as_array().unwrap().is_empty());

    let measured: Value = create(
        "Measured sauce",
        "For the base use @butter{20%g} and @butter{10%g}. Melt @butter{} in #saucepan{} for ~{1%minute}, then add @flour{15%g} and whisk in @milk{200%ml}.",
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let id = measured["recipe"]["id"].as_str().unwrap();
    let scaled: Value =
        bearer(reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}/scaled"))))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let ingredients = scaled["ingredients"].as_array().unwrap();
    assert_eq!(ingredients.len(), 5);
    assert_eq!(
        ingredients
            .iter()
            .map(|item| item["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["butter", "butter", "butter", "flour", "milk"]
    );
    assert_eq!(
        ingredients
            .iter()
            .map(|item| item["quantity"].as_str())
            .collect::<Vec<_>>(),
        [
            Some("20 g"),
            Some("10 g"),
            None,
            Some("15 g"),
            Some("200 ml")
        ]
    );
    let instructions = scaled["instructions"].as_array().unwrap();
    assert_eq!(instructions.len(), 1);
    assert!(instructions[0].as_str().unwrap().contains("saucepan"));
    assert!(instructions[0].as_str().unwrap().contains("1 minute"));
}

#[tokio::test]
async fn declaration_detection_is_structural_and_only_suppresses_block_references() {
    let app = support::TestApp::start().await;
    let create = |title: &str, body: &str| {
        bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
            .json(&json!({"title": title, "servings": 1, "body": body}))
    };

    let instruction: Value = create(
        "Fried onion",
        "@onion{1}, fry until golden\n\nServe the onion.",
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let id = instruction["recipe"]["id"].as_str().unwrap();
    let scaled: Value =
        bearer(reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}/scaled"))))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(scaled["ingredients"].as_array().unwrap().len(), 1);
    let instructions = scaled["instructions"].as_array().unwrap();
    assert_eq!(instructions.len(), 2);
    assert!(instructions[0]
        .as_str()
        .unwrap()
        .contains("fry until golden"));
    assert!(instructions[1]
        .as_str()
        .unwrap()
        .contains("Serve the onion"));

    let repeated: Value = create(
        "Repeated onions",
        "Add @onion{}(diced) to the pan. Stir in @onion{}(thinly sliced).",
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let id = repeated["recipe"]["id"].as_str().unwrap();
    let scaled: Value =
        bearer(reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}/scaled"))))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let ingredients = scaled["ingredients"].as_array().unwrap();
    assert_eq!(
        ingredients.len(),
        2,
        "ordinary repeated references are not deduplicated"
    );
    assert_eq!(ingredients[0]["note"], "diced");
    assert_eq!(ingredients[1]["note"], "thinly sliced");

    let declared_then_prepped: Value = create(
        "Onion occurrences",
        "@onion{1}\n@salt{1%pinch}\n\nAdd @onion{}(thinly sliced) to the pan.",
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let id = declared_then_prepped["recipe"]["id"].as_str().unwrap();
    let scaled: Value =
        bearer(reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}/scaled"))))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let ingredients = scaled["ingredients"].as_array().unwrap();
    assert_eq!(
        ingredients.len(),
        3,
        "a meaningful later occurrence note keeps its own row"
    );
    assert_eq!(ingredients[2]["name"], "onion");
    assert_eq!(ingredients[2]["note"], "thinly sliced");
}

#[tokio::test]
async fn public_browsing_filters_and_canonical_ingredient_directory_work_over_http() {
    let app = support::TestApp::start().await;
    let make = |title: &str, tags: Vec<&str>, body: &str| {
        bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
            .json(&json!({"title": title, "servings": 2, "tags": tags, "body": body}))
    };
    let soup: Value = make(
        "Chicken Soup",
        vec!["soup"],
        "@chicken{200%g}\n@chicken{50%g}\n@onion{1%piece}",
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let second: Value = make("Chicken Stew", vec!["soup", "dinner"], "@chicken{300%g}")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let unrelated: Value = make("Toast", vec!["quick"], "@bread{2%slice}")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    make(
        "<script>alert(1)</script>",
        vec!["soup"],
        "@pepper\n@chicken",
    )
    .send()
    .await
    .unwrap();
    make("Chicken Variant", vec!["dinner"], "@chickens{200%g}")
        .send()
        .await
        .unwrap();
    let soup_id = soup["recipe"]["id"].as_str().unwrap();
    let second_id = second["recipe"]["id"].as_str().unwrap();
    let unrelated_id = unrelated["recipe"]["id"].as_str().unwrap();

    let listing = app.get("/recipes?tag=soup").await;
    assert_eq!(
        listing.status(),
        200,
        "/recipes must be a public listing route"
    );
    let listing = listing.text().await.unwrap();
    assert!(listing.contains("<h2 class=\"card-title\">Chicken Soup</h2>"));
    assert!(listing.contains("<h2 class=\"card-title\">Chicken Stew</h2>"));
    assert!(listing.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!listing.contains("<script>alert(1)</script>"));
    assert!(!listing.contains("<h2 class=\"card-title\">Toast</h2>"));
    assert!(listing.contains("href=\"/recipes/"));
    assert!(app.get("/tags/soup").await.status().is_success());
    let matching = app
        .get("/recipes?q=Chicken&tag=soup")
        .await
        .text()
        .await
        .unwrap();
    assert!(matching.contains("<h2 class=\"card-title\">Chicken Soup</h2>"));
    assert!(matching.contains("<h2 class=\"card-title\">Chicken Stew</h2>"));
    assert!(!matching.contains("<h2 class=\"card-title\">Chicken Variant</h2>"));
    assert_eq!(matching.matches("Clear filters").count(), 1);
    let wrong_tag = app
        .get("/recipes?q=Chicken&tag=quick")
        .await
        .text()
        .await
        .unwrap();
    assert!(wrong_tag.contains("No recipes"));
    assert!(!wrong_tag.contains("<h2 class=\"card-title\">Chicken Soup</h2>"));

    let search = app.get("/?q=chicken&tag=").await;
    assert_eq!(search.status(), 200);
    let search = search.text().await.unwrap();
    assert!(search.contains("<h2 class=\"card-title\">Chicken Soup</h2>"));
    assert!(search.contains("<h2 class=\"card-title\">Chicken Stew</h2>"));
    assert!(search.contains("href=\"/recipes?tag=soup\">soup</a>"));
    assert!(search.contains("href=\"/ingredients\""));
    assert_eq!(search.matches("Clear filters").count(), 1);

    let whitespace_search = app
        .get("/recipes?q=%20Chicken%20&tag=%20soup%20")
        .await
        .text()
        .await
        .unwrap();
    assert!(whitespace_search.contains("<h2 class=\"card-title\">Chicken Soup</h2>"));
    assert!(whitespace_search.contains("value=\" Chicken \""));
    assert!(whitespace_search.contains("value=\" soup \""));
    assert_eq!(whitespace_search.matches("Clear filters").count(), 1);
    let blank_search = app
        .get("/recipes?q=%20%20&tag=%20")
        .await
        .text()
        .await
        .unwrap();
    assert!(blank_search.contains("<h2 class=\"card-title\">Chicken Soup</h2>"));
    assert!(!blank_search.contains("Clear filters"));
    let escaped_filter = app
        .get("/recipes?q=%3Cscript%3E")
        .await
        .text()
        .await
        .unwrap();
    assert!(escaped_filter.contains("value=\"&lt;script&gt;\""));
    assert!(!escaped_filter.contains("value=\"<script>\""));
    assert_eq!(escaped_filter.matches("Clear filters").count(), 1);

    let no_match = app
        .get("/recipes?q=does-not-exist&tag=soup")
        .await
        .text()
        .await
        .unwrap();
    assert!(no_match.contains("No recipes"));
    assert!(no_match.contains("Clear filters"));
    assert_eq!(no_match.matches("Clear filters").count(), 1);

    let directory = app.get("/ingredients").await;
    assert_eq!(directory.status(), 200);
    let directory = directory.text().await.unwrap();
    assert!(directory.contains("<title>Ingredients</title>"));
    assert!(directory.contains("<h1 class=\"collection-title\">Ingredients</h1>"));
    let chicken = directory
        .split("<h2>chicken</h2>")
        .nth(1)
        .unwrap()
        .split("</article>")
        .next()
        .unwrap();
    assert!(directory.contains("<h2>chicken</h2>"));
    let headings = directory
        .split("<h2>")
        .skip(1)
        .map(|section| section.split("</h2>").next().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        headings,
        ["bread", "chicken", "chickens", "onion", "pepper"]
    );
    assert!(chicken.contains("3 recipes"));
    assert!(chicken.contains(&format!("/recipes/{soup_id}")));
    assert!(chicken.contains(&format!("/recipes/{second_id}")));
    assert!(!chicken.contains(&format!("/recipes/{unrelated_id}")));
    assert!(directory.contains("<h2>onion</h2>"));
    let linked_id = chicken
        .split("href=\"/recipes/")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let linked_recipe = app.get(&format!("/recipes/{linked_id}")).await;
    assert_eq!(linked_recipe.status(), 200);
    let linked_recipe = linked_recipe.text().await.unwrap();
    assert!(linked_recipe.contains("<h1>"));

    let ingredients: Value = bearer(reqwest::Client::new().get(app.url("/api/v1/ingredients")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ingredient_id = |name: &str| {
        ingredients
            .as_array()
            .unwrap()
            .iter()
            .find(|ingredient| ingredient["name"] == name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let chicken_id = ingredient_id("chicken");
    let chickens_id = ingredient_id("chickens");
    let merged = bearer(
        reqwest::Client::new().post(app.url(&format!("/api/v1/ingredients/{chickens_id}/merge"))),
    )
    .json(&json!({"survivor": chicken_id}))
    .send()
    .await
    .unwrap();
    assert_eq!(merged.status(), 204);
    let directory = app.get("/ingredients").await.text().await.unwrap();
    let chicken = directory
        .split("<h2>chicken</h2>")
        .nth(1)
        .unwrap()
        .split("</article>")
        .next()
        .unwrap();
    assert!(chicken.contains("4 recipes"));
    assert!(!directory.contains("<h2>chickens</h2>"));
    assert!(directory.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!directory.contains("<script>alert(1)</script>"));

    let pepper_id = ingredient_id("pepper");
    let renamed =
        bearer(reqwest::Client::new().put(app.url(&format!("/api/v1/ingredients/{pepper_id}"))))
            .json(&json!({"name":"pepper <b>hot</b>"}))
            .send()
            .await
            .unwrap();
    assert_eq!(renamed.status(), 204);
    let directory = app.get("/ingredients").await.text().await.unwrap();
    assert!(directory.contains("pepper &lt;b&gt;hot&lt;/b&gt;"));
    assert!(!directory.contains("pepper <b>hot</b>"));

    let changed =
        bearer(reqwest::Client::new().put(app.url(&format!("/api/v1/recipes/{soup_id}"))))
            .json(&json!({
                "title":"Chicken Soup",
                "servings":2,
                "tags":["soup"],
                "body":"@rice{200%g}",
                "base_revision":soup["recipe"]["revision"]
            }))
            .send()
            .await
            .unwrap();
    assert_eq!(changed.status(), 200);
    let directory = app.get("/ingredients").await.text().await.unwrap();
    let chicken = directory
        .split("<h2>chicken</h2>")
        .nth(1)
        .unwrap()
        .split("</article>")
        .next()
        .unwrap();
    assert!(chicken.contains("3 recipes"));
    assert!(!chicken.contains(&format!("/recipes/{soup_id}")));

    let rev = second["recipe"]["revision"].as_str().unwrap();
    let deleted =
        bearer(reqwest::Client::new().delete(app.url(&format!("/api/v1/recipes/{second_id}"))))
            .json(&json!({"base_revision": rev}))
            .send()
            .await
            .unwrap();
    assert_eq!(deleted.status(), 200);
    let directory = app.get("/ingredients").await.text().await.unwrap();
    let chicken = directory
        .split("<h2>chicken</h2>")
        .nth(1)
        .unwrap()
        .split("</article>")
        .next()
        .unwrap();
    assert!(chicken.contains("2 recipes"));
    assert!(!chicken.contains(&format!("/recipes/{second_id}")));
}

#[tokio::test]
async fn public_and_api_pdf_routes_share_a_deterministic_single_page_a4_render() {
    let app = support::TestApp::start().await;
    let created = create(&app).await;
    let id = created["recipe"]["id"].as_str().unwrap();

    let public = app.get(&format!("/recipes/{id}.pdf?servings=4")).await;
    assert_eq!(public.status(), 200);
    assert_eq!(
        public.headers().get("content-type").unwrap(),
        "application/pdf"
    );
    let public = public.bytes().await.unwrap();
    assert!(public.starts_with(b"%PDF-"));
    let document = lopdf::Document::load_mem(&public).unwrap();
    let pages = document.get_pages();
    assert_eq!(pages.len(), 1, "PDF must contain exactly one page");
    let page = document
        .get_object(*pages.values().next().unwrap())
        .unwrap()
        .as_dict()
        .unwrap();
    let media_box = page.get(b"MediaBox").unwrap().as_array().unwrap();
    let width = media_box[2].as_float().unwrap();
    let height = media_box[3].as_float().unwrap();
    assert!((width - 595.276).abs() < 0.01);
    assert!((height - 841.89).abs() < 0.01);

    let api = bearer(
        reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}/pdf?servings=4"))),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(api.status(), 200);
    assert_eq!(api.bytes().await.unwrap(), public);

    let html = app
        .get(&format!("/recipes/{id}?servings=4"))
        .await
        .text()
        .await
        .unwrap();
    assert!(html.contains("400 g"));
}

#[tokio::test]
async fn pdf_uses_scaled_fractional_and_fixed_quantities_and_fits_long_recipes() {
    let app = support::TestApp::start().await;
    let recipe = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
        .json(&json!({
            "title": "Sauce",
            "servings": 2,
            "body": "@pepper{=1%g}\n@butter{20%g}\n@flour{10%g}\n@sugar{1%g}"
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["recipe"]
        .clone();
    let id = recipe["id"].as_str().unwrap();
    let html = app
        .get(&format!("/recipes/{id}?servings=3"))
        .await
        .text()
        .await
        .unwrap();
    assert!(html.contains("1 g"));
    assert!(html.contains("30 g"));
    assert!(html.contains("15 g"));
    assert!(html.contains("1 1/2 g"), "HTML was: {html}");

    let scaled: Value = bearer(
        reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}/scaled?servings=3"))),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let quantities = scaled["ingredients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|ingredient| ingredient["quantity"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(quantities, ["1 g", "30 g", "15 g", "1 1/2 g"]);

    let pdf = app
        .get(&format!("/recipes/{id}.pdf?servings=3"))
        .await
        .bytes()
        .await
        .unwrap();
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap();
    assert!(text.contains("1 g"), "PDF text was: {text}");
    assert!(text.contains("30 g"), "PDF text was: {text}");
    assert!(text.contains("15 g"), "PDF text was: {text}");
    assert!(text.contains("1 1/2 g"), "PDF text was: {text}");

    let long_body = (0..70)
        .map(|index| format!("Mix for step {index}: add a careful pinch of seasoning."))
        .collect::<Vec<_>>()
        .join("\n");
    let long = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
        .json(&json!({"title":"Long recipe","servings":2,"body":long_body}))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["recipe"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let fit: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes/validate")))
        .json(&json!({"title":"Long recipe","servings":2,"body":long_body}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fit["print_fit"]["fits"], true);
    assert!(fit["print_fit"]["font_size_pt"].as_u64().unwrap() < 11);

    let pdf = app.get(&format!("/recipes/{long}.pdf")).await;
    assert_eq!(pdf.status(), 200);
    let pdf = pdf.bytes().await.unwrap();
    assert_eq!(
        pdf_extract::extract_text_from_mem_by_pages(&pdf)
            .unwrap()
            .len(),
        1
    );
    let accepted_text = pdf_extract::extract_text_from_mem(&pdf).unwrap();
    assert!(
        accepted_text.contains("step 69"),
        "PDF text was: {accepted_text}"
    );
}

#[tokio::test]
async fn shared_outputs_preserve_timers_tiny_quantities_ranges_notes_tags_and_source() {
    let app = support::TestApp::start().await;
    let created = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
        .json(&json!({
            "title": "Details",
            "description": "Candidate description",
            "servings": 2,
            "tags": ["baking/sweet?yes#later"],
            "notes": "Rest before serving.",
            "source_url": "https://example.com/recipe",
            "body": "Add @salt{0.000001%g}(fine-grained), @pepper{1-2%g}, and @trace{0.0004-0.0008%g}.\nLet the dough rest for ~{1%hour}.\nAllow ~rest{2%minutes} before serving."
        }))
        .send().await.unwrap().json::<Value>().await.unwrap()["recipe"].clone();
    let id = created["id"].as_str().unwrap();
    let html = app
        .get(&format!("/recipes/{id}"))
        .await
        .text()
        .await
        .unwrap();
    assert!(html.contains("fine-grained"));
    assert!(html.contains("0.000001 g"), "{html}");
    assert!(
        html.contains("1-2 g") || html.contains("1 to 2 g"),
        "{html}"
    );
    assert!(html.contains("0.0004-0.0008 g"), "{html}");
    assert!(html.contains("1 hour"), "{html}");
    assert!(html.contains("rest (2 minutes)"), "{html}");
    assert!(html.contains("Rest before serving."));
    assert!(html.contains("https://example.com/recipe"));
    assert!(html.contains("/recipes?tag=baking%2Fsweet%3Fyes%23later"));
    assert!(!html.contains("/tags/baking/"));

    let pdf = app
        .get(&format!("/recipes/{id}.pdf"))
        .await
        .bytes()
        .await
        .unwrap();
    let text = pdf_extract::extract_text_from_mem(&pdf)
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.contains("1 hour") || text.contains("1hour"), "{text}");
    for expected in [
        "0.000001 g",
        "1-2 g",
        "0.0004-0.0008 g",
        "rest",
        "2 minutes",
        "fine-grained",
        "Rest before serving.",
    ] {
        assert!(text.contains(expected), "missing {expected:?}: {text}");
    }

    let candidate = json!({
        "title": "Candidate",
        "description": "Full candidate description",
        "servings": 2,
        "body": "@rice{100%g}\nLet it rest for ~{5%minutes}.",
        "notes": "Candidate note.",
        "source_url": "https://example.org/source",
        "tags": ["rice"],
        "target_servings": 3
    });
    let fit: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes/validate")))
        .json(&candidate)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fit["print_fit"]["fits"], true);
    let actual = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
        .json(&candidate)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let actual_id = actual["recipe"]["id"].as_str().unwrap();
    let actual_pdf = bearer(
        reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{actual_id}/pdf?servings=3"))),
    )
    .send()
    .await
    .unwrap()
    .bytes()
    .await
    .unwrap();
    let pdf_text = pdf_extract::extract_text_from_mem(&actual_pdf).unwrap();
    assert!(pdf_text.contains("Full candidate description"));
    assert!(pdf_text.contains("Candidate note."));
    assert!(pdf_text.contains("5 minutes"));
}

#[tokio::test]
async fn oversized_recipe_returns_structured_422_without_a_partial_pdf() {
    let app = support::TestApp::start().await;
    let body = (0..300)
        .map(|index| format!("Step {index}: {}", "carefully stir and cook ".repeat(14)))
        .collect::<Vec<_>>()
        .join("\n");
    let id = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
        .json(&json!({"title":"Impossible recipe","servings":2,"body":body.clone()}))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["recipe"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let response = app.get(&format!("/recipes/{id}.pdf")).await;
    assert_eq!(response.status(), 422);
    let error: Value = response.json().await.unwrap();
    assert_eq!(error["code"], "print_does_not_fit");
    assert_eq!(error["details"]["minimum_font_size_pt"], 8);
    assert!(
        error["details"]["page_count_at_minimum_font_size"]
            .as_u64()
            .unwrap()
            > 1
    );

    let validation: Value =
        bearer(reqwest::Client::new().post(app.url("/api/v1/recipes/validate")))
            .json(&json!({"title":"Impossible recipe","servings":2,"body":body}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(validation["print_fit"]["fits"], false);
    assert_eq!(validation["print_fit"]["font_size_pt"], Value::Null);
    assert_eq!(validation["print_fit"]["status"], "does_not_fit");
}

#[tokio::test]
async fn pdf_embeds_attached_picture() {
    let app = support::TestApp::start().await;
    let recipe = create(&app).await;
    let recipe = &recipe["recipe"];
    let id = recipe["id"].as_str().unwrap();
    let mut png = Cursor::new(Vec::new());
    image::RgbImage::from_pixel(24, 24, image::Rgb([220, 30, 40]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let uploaded: Value = bearer(
        reqwest::Client::new()
            .post(app.url("/api/v1/images"))
            .header("content-type", "image/png")
            .body(png.into_inner()),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let _: Value =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/recipes/{id}/image"))))
            .json(&json!({
                "base_revision": recipe["revision"],
                "image_ref": uploaded["ref"]
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

    let fit: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes/validate")))
        .json(&json!({
            "title": recipe["title"],
            "description": recipe["description"],
            "servings": recipe["servings"],
            "tags": ["quick"],
            "body": "@butter{20%g}\n@pasta{200%g}",
            "image_ref": uploaded["ref"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fit["print_fit"]["fits"], true);

    let pdf = app
        .get(&format!("/recipes/{id}.pdf"))
        .await
        .bytes()
        .await
        .unwrap();
    assert!(
        pdf.windows(b"/Subtype /Image".len())
            .any(|window| window == b"/Subtype /Image"),
        "attached picture must be embedded in the PDF"
    );
}

#[tokio::test]
async fn authenticated_crud_keeps_immutable_history_and_rejects_stale_writes() {
    let app = support::TestApp::start().await;
    let (stored_hash, label) = app.stored_token().await;
    assert_eq!(label, "integration");
    assert_eq!(stored_hash.len(), 64);
    assert_ne!(stored_hash, "test-secret");
    assert_eq!(app.get("/api/v1/recipes").await.status(), 401);
    let unauthenticated_write = reqwest::Client::new()
        .post(app.url("/api/v1/recipes"))
        .json(&json!({"title":"x","servings":1,"body":"@salt"}))
        .send()
        .await
        .expect("unauthenticated request");
    assert_eq!(unauthenticated_write.status(), 401);

    let first = create(&app).await;
    let id = first["recipe"]["id"].as_str().unwrap();
    let rev1 = first["recipe"]["revision"].as_str().unwrap();
    assert_eq!(first["recipe"]["author_label"], "integration");
    assert!(first["recipe"]["revision_created_at"]
        .as_str()
        .unwrap()
        .contains('T'));

    let update = json!({
        "title": "Butter noodles",
        "description": "Updated",
        "servings": 3,
        "tags": ["quick", "edited"],
        "body": "@butter{30%g}\n@pasta{300%g}",
        "base_revision": rev1
    });
    let response = bearer(reqwest::Client::new().put(app.url(&format!("/api/v1/recipes/{id}"))))
        .json(&update)
        .send()
        .await
        .expect("update request");
    assert_eq!(response.status(), 200);
    let second: Value = response.json().await.unwrap();
    let rev2 = second["recipe"]["revision"].as_str().unwrap();

    let second_update = json!({
        "title": "Butter noodles",
        "description": "Edited a second time",
        "servings": 4,
        "tags": ["quick", "edited"],
        "body": "@butter{40%g}\n@pasta{400%g}",
        "base_revision": rev2
    });
    let response = bearer(reqwest::Client::new().put(app.url(&format!("/api/v1/recipes/{id}"))))
        .json(&second_update)
        .send()
        .await
        .expect("second update request");
    assert_eq!(response.status(), 200);
    let third: Value = response.json().await.unwrap();
    let rev3 = third["recipe"]["revision"].as_str().unwrap();

    let stale = bearer(reqwest::Client::new().put(app.url(&format!("/api/v1/recipes/{id}"))))
        .json(&update)
        .send()
        .await
        .expect("stale request");
    assert_eq!(stale.status(), 409);
    let stale: Value = stale.json().await.unwrap();
    assert_eq!(stale["code"], "conflict");
    assert_eq!(stale["current_revision_id"], rev3);

    let history: Value =
        bearer(reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}/revisions"))))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(history.as_array().unwrap().len(), 3);
    let historical: Value = bearer(
        reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}/revisions/{rev1}"))),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(historical["description"], "A simple supper");

    let deleted: Value =
        bearer(reqwest::Client::new().delete(app.url(&format!("/api/v1/recipes/{id}"))))
            .json(&json!({"base_revision": rev3}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(deleted["deleted"], true);
    assert_eq!(app.get(&format!("/recipes/{id}.pdf")).await.status(), 404);
    let list: Value = bearer(reqwest::Client::new().get(app.url("/api/v1/recipes")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.as_array().unwrap().is_empty());

    let restored: Value =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/recipes/{id}/restore"))))
            .json(&json!({"base_revision": deleted["revision"]}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(restored["deleted"], false);
    assert_ne!(restored["revision"], rev3);
    let history: Value =
        bearer(reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}/revisions"))))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(history.as_array().unwrap().len(), 5);

    let restored_old: Value = bearer(
        reqwest::Client::new().post(app.url(&format!("/api/v1/recipes/{id}/revisions/{rev1}"))),
    )
    .json(&json!({"base_revision": restored["revision"]}))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(restored_old["description"], "A simple supper");
    assert_ne!(restored_old["revision"], rev1);
}

#[tokio::test]
async fn only_one_concurrent_update_from_the_same_base_can_win() {
    let app = support::TestApp::start().await;
    let created = create(&app).await;
    let id = created["recipe"]["id"].as_str().unwrap();
    let base = created["recipe"]["revision"].as_str().unwrap();
    let client = reqwest::Client::new();
    let left = bearer(client.put(app.url(&format!("/api/v1/recipes/{id}")))).json(&json!({
        "title": "Left edit",
        "servings": 2,
        "body": "@rice{100%g}",
        "base_revision": base
    }));
    let right = bearer(client.put(app.url(&format!("/api/v1/recipes/{id}")))).json(&json!({
        "title": "Right edit",
        "servings": 3,
        "body": "@rice{150%g}",
        "base_revision": base
    }));
    let (left, right) = tokio::join!(left.send(), right.send());
    let statuses = [
        left.expect("left update").status().as_u16(),
        right.expect("right update").status().as_u16(),
    ];
    assert_eq!(statuses.iter().filter(|status| **status == 200).count(), 1);
    assert_eq!(statuses.iter().filter(|status| **status == 409).count(), 1);
    let history: Value =
        bearer(reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}/revisions"))))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(history.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn stale_revision_wins_over_invalid_cooklang_and_invalid_current_update_rolls_back() {
    let app = support::TestApp::start().await;
    let created = create(&app).await;
    let id = created["recipe"]["id"].as_str().unwrap();
    let base = created["recipe"]["revision"].as_str().unwrap();

    let first_update =
        bearer(reqwest::Client::new().put(app.url(&format!("/api/v1/recipes/{id}"))))
            .json(&json!({
                "title": "Committed",
                "servings": 2,
                "body": "@rice{100%g}",
                "base_revision": base
            }))
            .send()
            .await
            .unwrap();
    assert_eq!(first_update.status(), 200);
    let current_revision = first_update.json::<Value>().await.unwrap()["recipe"]["revision"]
        .as_str()
        .unwrap()
        .to_owned();

    let stale_invalid =
        bearer(reqwest::Client::new().put(app.url(&format!("/api/v1/recipes/{id}"))))
            .json(&json!({
                "title": "Invalid stale edit",
                "servings": 2,
                "body": "~rest{}",
                "base_revision": base
            }))
            .send()
            .await
            .unwrap();
    assert_eq!(stale_invalid.status(), 409);
    let error: Value = stale_invalid.json().await.unwrap();
    assert_eq!(error["code"], "conflict");
    assert_eq!(error["current_revision_id"], current_revision);

    let invalid_current =
        bearer(reqwest::Client::new().put(app.url(&format!("/api/v1/recipes/{id}"))))
            .json(&json!({
                "title": "Invalid current edit",
                "servings": 2,
                "body": "~rest{}",
                "base_revision": current_revision
            }))
            .send()
            .await
            .unwrap();
    assert_eq!(invalid_current.status(), 422);
    let current: Value =
        bearer(reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}"))))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(current["revision"], current_revision);
    assert_eq!(current["title"], "Committed");
}

#[tokio::test]
async fn structured_recipe_validation_and_guidelines_are_available() {
    let app = support::TestApp::start().await;
    let invalid_cooklang = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
        .json(&json!({
            "title": "Invalid body",
            "servings": 1,
            "body": "First step\n~rest{}"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid_cooklang.status(), 422);
    let diagnostic: Value = invalid_cooklang.json().await.unwrap();
    assert_eq!(diagnostic["code"], "cooklang_invalid");
    assert_eq!(diagnostic["diagnostics"][0]["line"], 2);
    assert!(diagnostic["diagnostics"][0]["column"].as_u64().is_some());
    assert!(!diagnostic["diagnostics"][0]["message"]
        .as_str()
        .unwrap()
        .is_empty());
    let unchanged: Value = bearer(reqwest::Client::new().get(app.url("/api/v1/recipes")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(unchanged.as_array().unwrap().is_empty());

    for body in [
        "---\ntitle: duplicated\n---\n@rice{100%g}",
        ">> servings: 1\n@rice{100%g}",
    ] {
        let rejected = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
            .json(&json!({
                "title": "Cooklang metadata",
                "servings": 1,
                "body": body
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), 422);
        let error: Value = rejected.json().await.unwrap();
        assert_eq!(error["code"], "cooklang_metadata_forbidden");
    }

    let malformed_yaml = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
        .json(&json!({
            "title": "Malformed Cooklang metadata",
            "servings": 1,
            "body": "---\ntitle: duplicated\n---\n~rest{}"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(malformed_yaml.status(), 422);
    let yaml_error: Value = malformed_yaml.json().await.unwrap();
    assert_eq!(yaml_error["code"], "cooklang_invalid");
    assert!(!yaml_error["diagnostics"].as_array().unwrap().is_empty());

    let malformed_json = reqwest::Client::new()
        .post(app.url("/api/v1/recipes"))
        .bearer_auth("test-secret")
        .header("content-type", "application/json")
        .body(r#"{"title":"incomplete""#)
        .send()
        .await
        .unwrap();
    assert_eq!(malformed_json.status(), 400);
    let error: Value = malformed_json.json().await.unwrap();
    assert_eq!(error["code"], "invalid_json");
    assert!(!error["message"].as_str().unwrap().is_empty());

    let incomplete_json = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
        .json(&json!({"title": "Missing required fields"}))
        .send()
        .await
        .unwrap();
    assert_eq!(incomplete_json.status(), 400);
    let error: Value = incomplete_json.json().await.unwrap();
    assert_eq!(error["code"], "invalid_json");

    let created = create(&app).await;
    assert_eq!(created["warnings"].as_array().unwrap().len(), 0);
    let guideline = bearer(reqwest::Client::new().get(app.url("/api/v1/guidelines")))
        .send()
        .await
        .unwrap();
    assert_eq!(guideline.status(), 200);
    let text = guideline.text().await.unwrap();
    assert!(text.contains("temperatures"));
    assert!(text.contains("Never invent"));

    let openapi = app.get("/api/v1/openapi.yaml").await;
    assert_eq!(openapi.status(), 200);
    let openapi = openapi.text().await.unwrap();
    assert!(openapi.contains("openapi: 3.1.0"));
    assert!(openapi.contains("RecipeDocument"));
    assert!(openapi.contains("SaveResult"));
    assert!(openapi.contains("current_revision_id"));
    assert!(openapi.contains("author_label"));
    assert!(openapi.contains("Cooklang parser warnings"));
}
