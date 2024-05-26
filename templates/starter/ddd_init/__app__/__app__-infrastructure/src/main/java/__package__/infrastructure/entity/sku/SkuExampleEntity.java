package __package__.infrastructure.entity.sku;

/**
 * SkuExampleEntity
 *
 * @author weirenyan
 * @date 2023/12/6
 */
public class SkuExampleEntity {
    /** skuId */
    private String skuId;
    /** sku名称 */
    private String skuName;

    /** 所属一级类目编码 */
    private String firstCategoryCode;

    /** 所属一级类目名称 */
    private String firstCategoryName;

    /** 所属二级类目编码 */
    private String secondCategoryCode;

    /** 所属二级类目名称 */
    private String secondCategoryName;

    /**
     * specifications
     * 重量规格
     */
    private Long  specifications;

    /**
     * originAddress
     * 产地
     */
    private String  originAddress;


    /**
     * 商品短标题
     */
    private String  shortTitle;

}
